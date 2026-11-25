//! Writing the `venue_account_id` (book) column: normalisation, `BookWrite`, `BookSource`.

use super::model::{account_from_row, account_select};
use super::*;

// ---------------------------------------------------------------------------------------------
// Writing — the BOOK column
// ---------------------------------------------------------------------------------------------

/// The longest `venue_account_id` this store will accept.
///
/// Measured against the widest shape any roster venue actually names a book with: a
/// hyperliquid/aster EVM address is 42 characters (`0x` + 20 bytes hex), and every other venue in
/// `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §7 answers with something shorter —
/// dukascopy's measured numbers are seven digits, IBKR's is `DU` plus six. The cap is not a wire
/// constraint and nothing downstream depends on it; it is a floor under [`normalized_venue_account_id`]
/// so a whole file pasted into the flag cannot become a row.
pub const VENUE_ACCOUNT_ID_MAX_BYTES: usize = 128;

/// **Invisible at both edges of a paste** — whitespace, control characters, and the FORMAT
/// characters neither of those two predicates covers.
///
/// ⚠ **`char::is_control` is category Cc ALONE and `char::is_whitespace` is the `White_Space`
/// property, so between them they classify none of Cf** — and `str::trim` strips only the second.
/// U+FEFF (a byte-order mark), U+200B (a zero-width space) and U+200E (a left-to-right mark)
/// therefore survive both a `trim` and a `is_whitespace() || is_control()` test, which is exactly
/// what [`normalized_venue_account_id`] used to apply. A value pasted off a venue's own page with a
/// leading BOM would have been STORED with it: identical to another row's book on every screen an
/// operator can read, and UNEQUAL to it wherever two books are compared (the start's shared-book
/// warning, `vike_dukascopy`'s `resolve_account`), so a character nobody can see would decide what
/// matches. That is the failure this predicate exists for, and it is why the cure is not "strip the
/// whitespace harder".
///
/// The list is the Cf characters that can plausibly ride along on a copy — the bidi marks and
/// embeddings, the zero-width family, the word joiner and invisible operators, the soft hyphen and
/// the Mongolian vowel separator. It does not need to be the whole of Cf: an INTERIOR one is
/// refused by [`normalized_venue_account_id`]'s ASCII-graphic rule whether it is named here or not,
/// so this list only has to cover what a paste can leave at an EDGE.
fn is_invisible(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || matches!(
            c,
            '\u{00AD}'                  // SOFT HYPHEN
            | '\u{061C}'                // ARABIC LETTER MARK
            | '\u{180E}'                // MONGOLIAN VOWEL SEPARATOR
            | '\u{200B}'..='\u{200F}'   // ZWSP, ZWNJ, ZWJ, LRM, RLM
            | '\u{202A}'..='\u{202E}'   // the bidi embeddings and overrides
            | '\u{2060}'..='\u{2064}'   // WORD JOINER and the invisible operators
            | '\u{2066}'..='\u{2069}'   // the bidi isolates
            | '\u{FEFF}'                // ZERO WIDTH NO-BREAK SPACE — a pasted byte-order mark
        )
}

/// **The ONE predicate for what may be a `venue_account_id`** — the cleaned value, or `None`.
///
/// Surrounding INVISIBLES are TRIMMED rather than refused ([`is_invisible`] — whitespace, control
/// characters AND the format characters `str::trim` leaves behind): an operator reads the number off
/// the venue's own page and pastes it, and a trailing space or a leading byte-order mark is not a
/// different account. Everything else is a refusal, and the reason is that this column is not free
/// text — it is the value two accounts are compared by (the shared-book warning, a dukascopy
/// address), so `"1234567 "` and `"1234567"` matching or NOT matching would be decided by an
/// invisible byte.
///
/// **What survives the trim must be ASCII GRAPHIC end to end** (`0x21..=0x7E`), which is a stronger
/// rule than the `is_whitespace() || is_control()` one it replaces and is stronger on purpose. It
/// refuses, in one test: interior whitespace (a book identifier is one token — two tokens means a
/// label was pasted beside the number), every control character, every FORMAT character (an
/// interior zero-width space is the attack the trim above cannot reach), and every non-ASCII
/// character at all — which closes the homoglyph case, where a Cyrillic `о` renders exactly like a
/// Latin `o` and compares unequal to it.
///
/// ⚠ **The ASCII rule is a REFUSAL, not a mangling, and widening it is a one-line change with a
/// measurement behind it.** Every book shape §7 of
/// `docs/superpowers/specs/2026-09-14-the-credential-schema.md` measured is ASCII — a decimal
/// account number, an `0x` EVM address, a `DU`-prefixed IBKR id, a venue login — so a venue that
/// genuinely answers with a non-ASCII identifier is a case nobody has met yet. It arrives here as a
/// loud refusal rather than as a row that quietly does not compare equal to itself, which is the
/// right way round for a column that decides which broker an order reaches.
///
/// Also refused: empty after the trim, and anything over [`VENUE_ACCOUNT_ID_MAX_BYTES`] (measured
/// AFTER the trim, so a padded paste is not refused for the padding).
///
/// ⚠ It is `pub` so the CLI's early refusal and this module's write-path refusal are ONE predicate
/// with two messages. They deliberately do NOT share a message: the CLI names the flag the operator
/// typed, and this module names the store — and neither ever echoes the offending token, because
/// nothing here can know that a value which failed this predicate was not a secret pasted into the
/// wrong flag.
#[must_use]
pub fn normalized_venue_account_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim_matches(is_invisible);
    if trimmed.is_empty() || trimmed.len() > VENUE_ACCOUNT_ID_MAX_BYTES {
        return None;
    }
    if !trimmed.chars().all(|c| c.is_ascii_graphic()) {
        return None;
    }
    Some(trimmed.to_string())
}

/// **What [`set_venue_account_id`] did** — the row AS IT WAS BEFORE the write, and what it holds now.
///
/// The `before` row is the point of the type. A `venue_account_id` decides which BROKER an order
/// routes to — dukascopy's two demo accounts are Dukascopy Bank SA and Dukascopy Europe IBS AS, two
/// legal entities — so a caller must be able to say WHICH ROW it changed in the same breath as
/// saying it changed one, out of the transaction that actually performed the write rather than out
/// of a read it did beforehand.
///
/// ⚠ No value, no secret, no credential — the same property [`AccountRow`] has, and by the same
/// construction: this type is built from the `account` table alone, and `venue_account_id` is an
/// account number the venue echoes on its own wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookWrite {
    /// The account row as the write transaction found it — including its previous
    /// [`AccountRow::venue_account_id`], which is `None` on every row a migration wrote.
    pub before: AccountRow,
    /// The value the row carries now, after [`normalized_venue_account_id`] — or `None` when the
    /// call was a CLEAR and the row is back to *not yet known*.
    pub venue_account_id: Option<String>,
    /// `false` when the row already held exactly this BOOK and no book was written. Idempotent
    /// rather than an error: re-running the same command is not a mistake, and refusing it would
    /// make a script that re-asserts a known book fail on its second run. A CLEAR of a row that
    /// already named no book reports `false` for the same reason.
    ///
    /// ⚠ It is a claim about the `venue_account_id` COLUMN alone, so a confirming handshake write
    /// reports `false` here and a `Some` in [`BookWrite::verified_at`] — the row was written, the
    /// book did not move. Read the two together; neither alone is *nothing happened*.
    pub changed: bool,
    /// What this write stamped into `last_verified_at`, or `None` when it stamped nothing.
    ///
    /// `Some` exactly under [`BookSource::Handshake`], carrying the instant that arm supplied. It
    /// is on the result rather than left for the caller to remember, because the confirmation case
    /// — book unchanged, timestamp written — is otherwise indistinguishable from a true no-op in
    /// everything this type reports.
    pub verified_at: Option<String>,
}

/// **WHO is telling the store this book, and therefore whether `last_verified_at` may be stamped.**
///
/// A parameter rather than two functions, and that is the load-bearing choice:
/// `crates/vike-ops/tests/settings_secrets/credential_writer_gate.rs` pins the SET of names that write this store,
/// and a second write function for the timestamp is precisely what that gate exists to refuse. One
/// writer, told what it is doing.
///
/// # Why the distinction exists at all
///
/// [`set_venue_account_id`]'s doc used to carry a *What it does NOT write* section saying
/// `last_verified_at` stays untouched, because *"an operator typing a number read off a web page has
/// performed [no authenticated session] from this process — stamping it here would make a
/// hand-entered row indistinguishable from one a handshake confirmed, which is the false confidence
/// §1 of the spec is about."* That argument is not weakened by this enum; it is what the enum
/// ENCODES. [`BookSource::Operator`] is that sentence, and it is still the behaviour of every
/// operator-facing door.
///
/// What changed is that a SECOND caller now exists whose claim is the other one —
/// `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §4.5's handshake writer, folding
/// what a venue answered during a session that really did authenticate. A `bool` would have carried
/// the same bit and none of the argument; a named arm makes the wrong one impossible to pass by
/// accident and impossible to pass without reading why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookSource<'a> {
    /// **An OPERATOR supplied this number** — typed at a CLI, read off the venue's own web page.
    /// `last_verified_at` is left exactly as it was: nothing in this process authenticated, and a
    /// hand-entered row that read *verified* would be a row that looks confirmed and is not.
    Operator,
    /// **A VENUE's own successful authenticated handshake answered with it**, at this instant —
    /// an RFC 3339 UTC string the caller formats (this crate has no time dependency).
    ///
    /// Stamps `last_verified_at`, and stamps it **even when the book is unchanged**: *the row
    /// already says what the venue says* is the CONFIRMATION case, and it is the one the column
    /// exists for. A write that only stamped on a change would leave a correctly-configured account
    /// looking never-verified forever.
    Handshake {
        /// The instant the HANDSHAKE succeeded — not the instant of this call. See
        /// [`AccountRow::last_verified_at`].
        verified_at: &'a str,
    },
}

impl BookSource<'_> {
    /// The value to write into `last_verified_at`, or `None` to leave the column alone.
    fn verified_at(&self) -> Option<&str> {
        match self {
            BookSource::Operator => None,
            BookSource::Handshake { verified_at } => Some(verified_at),
        }
    }
}

/// **Write ONE account row's `venue_account_id`** — the column
/// `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §4.5 records as having no writer in
/// this tree at all.
///
/// # Which of the column's sources this is, and which it is NOT
///
/// §4.5 names two writers for this column and this function is neither of them, which is the first
/// thing to know before reading it:
///
/// * **the migration's FOLD (§11 step 3)** — ten stored keys that already ARE the book
///   (`OANDA_DEMO_ACCOUNT_ID`, `ALPACA_SANDBOX_ACCOUNT_ID`, `CTRADER_DEMO_ACCOUNT_ID`,
///   `IBKR_DEMO_ACCOUNT`, `HYPERLIQUID_{DEMO,LIVE}_ACCOUNT_ADDRESS`, `ASTER_LIVE_USER`,
///   `IG_DEMO_IDENTIFIER`, `FXCM_DEMO_USER`, `POLY_FUNDER`) move into this column at migration
///   time and no `credential` row is written for them. **That fold is NOT performed by this
///   function and is not shipped**: §7 states its sequencing requirement outright — four of those
///   ten are load-bearing for AUTHENTICATION rather than for labelling, so the map RENDERER that
///   re-synthesizes their legacy names out of this column must be covered BEFORE the fold ships,
///   and §12 records that renderer as owed. `crate::schema`'s migration path is where the fold
///   will live when it does;
/// * **the venue's own HANDSHAKE** — every mount already performs an authenticated handshake that
///   carries the account identifier. ⚠ This read *"and nothing in this tree records what it
///   learned. Also owed (§12)"* until 2026-09-15. It is built: this same function performs it,
///   under [`BookSource::Handshake`], which is why that arm exists rather than a second writer.
///   What reaches it is a fold of the confirmations a mount parks
///   (`vike_secrets::account_confirmation`), because the deployed daemon's sandbox cannot write this
///   database at all — that module's doc carries the measurement.
///
/// This is the THIRD source, and it exists because one venue's books are reachable by neither: the
/// two dukascopy demo accounts are `(dukascopy, demo, label = NULL)` twice over, they differ only
/// by `id`, and their numbers are not in the credential store and not derivable from anything in
/// it. They were read off the venue by logging in. So an operator supplies them, one row at a time,
/// and this is the door — an operator standing in for the handshake §4.5 describes, by hand.
///
/// ⚠ **The numbers themselves are NOT in this workspace and may never be.** They are one operator's
/// account data: a source literal would be wrong for every other operator and would ship somebody's
/// account numbers into the public mirror.
///
/// # `venue_account_id: None` is a CLEAR
///
/// `None` puts the column back to `NULL` — *not yet known*, the state every migrated row starts in
/// — and it is the only move on this path that makes no assertion about a broker: a row whose book
/// is simply wrong is taken back to blank without first naming another number.
///
/// ⚠ It used to be the only way out of a pair written the wrong way round, when ruling 11's index
/// refused every direct correction of the crossed state. Decision 0274 removed that index, so each
/// row is now corrected with `replace` directly; the clear stays for the row that names no book.
///
/// A clear asks no guard — see the comment at the branch. Clearing a row that already names no
/// book is a no-op reported as `changed: false`, the same shape as re-writing a known value.
///
/// # What it refuses, and why each refusal is a refusal rather than a guess
///
/// ⚠ Every refusal in this list is asked of a SET. A CLEAR reaches none of them but two: the `id`
/// and the vanished database.
///
/// * **a value the predicate will not take** — [`normalized_venue_account_id`];
/// * **an `id` no row carries** ([`StoreErrorKind::NoSuchAccount`]) — never a create. This function
///   cannot bring an account into existence: `id` is the identity, and inventing a row for an id
///   the operator mistyped is how a book lands on an account nobody has;
/// * **a row that already names a DIFFERENT book** ([`StoreErrorKind::BookAlreadyKnown`]), unless the
///   caller passes `replace`. ⚠ This is the interlock that matters. Overwriting a known book with a
///   different number re-points an armed account at another broker, in silence, and a mistyped `id`
///   is exactly how that happens — so the default is refusal, the refusal names the row and the
///   number it already holds, and the caller has to say out loud that the stored number is the
///   wrong one. Re-writing the SAME value is not a change and is reported as `changed: false`.
///   "Different" is decided over `vike_model::BookId`s: the same book in another letter case
///   (`du1234567` over `DU1234567`) is not refused, and the new spelling is written;
/// * ⚠ **NOT a book another row names.** The venue account number is a field, never a uniqueness
///   key (decision 0274, which retired ruling 11's `account_one_account_per_book` index): one
///   wallet under a `demo` and a `live` row, or N labelled rows on one venue account, are ordinary.
///   Two ACTIVE rows of one venue AND one tier naming one book are reported at the start
///   (`vike_config::shared_books`), never refused here;
/// * **a database that VANISHED** between the backend choice and this open
///   ([`StoreErrorKind::VanishedDatabase`]) — the same invariant [`upsert_rows`] states at length, and
///   it is not weaker here: [`open_for_write`] carries no create flag, so a `set-book` against a box
///   with no database at all is refused rather than MINTING one holding a single account row and
///   nothing else, from which moment [`crate::store::backend_at`] would answer `Database` for every
///   process on the box. [`create_store`] stays the only function here that may bring a database
///   into existence.
///
/// # What it writes besides the book, and what it never writes
///
/// `last_verified_at` is written **only** under [`BookSource::Handshake`], and the whole argument
/// for that split is on [`BookSource`] itself. Under [`BookSource::Operator`] — every
/// operator-facing door, `vike-cli account set-book` included — the column stays untouched, exactly
/// as it did before that parameter existed: §4.5 gives it to *a successful authenticated session*,
/// and an operator typing a number read off a web page has performed none from this process.
///
/// ⚠ A `Handshake` write stamps the timestamp **on the unchanged path too** — the
/// `before.venue_account_id == value` early return still writes, and still reports
/// `changed: false` for the BOOK. That is the confirmation case, and it is the case the column
/// exists for; a stamp that required a book change would leave a correctly-configured account
/// reading *never verified* forever. ⚠ It is NOT stamped on a refusal of any kind: a refusal is a
/// transaction that wrote nothing, and a session whose identity claim the store just refused is the
/// single row that must not read *verified*.
///
/// `label` stays untouched under every source, and deliberately: the owner refused the provisional
/// `DEMO1`/`DEMO2` spellings at signature, and this whole column exists so that identity does not
/// have to live in a name.
///
/// Nothing else in the store is read or written: one `UPDATE` of one row, in one transaction, with
/// every `credential` row untouched.
#[cfg_attr(not(feature = "test-support"), expect(unreachable_pub))]
pub fn set_venue_account_id(
    path: &Path,
    id: i64,
    venue_account_id: Option<&str>,
    replace: bool,
    source: BookSource<'_>,
) -> Result<BookWrite, StoreError> {
    let refuse = |kind| StoreError { path: path.to_path_buf(), kind };
    let value: Option<String> = match venue_account_id {
        Some(raw) => match normalized_venue_account_id(raw) {
            Some(v) => Some(v),
            None => return Err(refuse(StoreErrorKind::BookMalformed)),
        },
        // A CLEAR. There is no value to validate, and none of the refusals below apply to it — see
        // the CLEAR section of this function's doc.
        None => None,
    };

    // ⚠ See the refusal list above: a store that is not there is refused, and nothing is created.
    let Some(mut conn) = open_for_write(path)? else {
        return Err(refuse(StoreErrorKind::VanishedDatabase));
    };

    // ⚠ **IMMEDIATE, not the default DEFERRED, and this is the whole of the concurrency story.**
    // Everything below is a read-then-write: the row is SELECTed, the refusals are decided from
    // what it holds, and only then is the UPDATE issued. A DEFERRED transaction takes no lock until
    // its first statement, so it acquires SHARED on that SELECT and must PROMOTE to RESERVED at the
    // UPDATE — and when another connection already holds RESERVED, SQLite refuses that promotion
    // with `SQLITE_BUSY` **immediately, without consulting the busy handler at all**, because
    // sleeping there could deadlock two waiters. So [`BUSY_TIMEOUT`] cannot cover this case and no
    // amount of raising it would: a second writer arriving mid-decision surfaced as a bare
    // `database is locked` no matter what. IMMEDIATE takes RESERVED up front, which is the one form
    // of this transaction the busy handler can actually wait on — and it also makes the decision
    // and the write ATOMIC against another writer rather than merely likely to be.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| StoreError::sql(path, e))?;

    // The row, read INSIDE the transaction that will write it — so the echo a caller renders is
    // what the write actually saw, not what a read beforehand happened to find.
    let select = account_select(&tx).map_err(|e| StoreError::sql(path, e))?;
    let before = tx
        .query_row(&format!("{select} WHERE a.id = ?1"), [id], account_from_row)
        .optional()
        .map_err(|e| StoreError::sql(path, e))?;
    let Some(before) = before else {
        return Err(refuse(StoreErrorKind::NoSuchAccount { id }));
    };

    if before.venue_account_id == value {
        // Already exactly this — including a CLEAR of a row that names no book. Nothing to write
        // about the BOOK, and not an error; see `BookWrite::changed`.
        //
        // ⚠ **A HANDSHAKE still stamps here, and this is the CONFIRMATION case** — the row already
        // says what the venue says, which is the single most useful thing `last_verified_at` can
        // record. Before this arm existed the function returned early and a correctly-configured
        // account would have read *never verified* forever, however many sessions it authenticated.
        // `changed` stays `false`, because it is a claim about the BOOK and the book did not move.
        if let Some(verified_at) = source.verified_at() {
            tx.execute("UPDATE account SET last_verified_at = ?2 WHERE id = ?1", (id, verified_at))
                .map_err(|e| StoreError::sql(path, e))?;
            tx.commit().map_err(|e| StoreError::sql(path, e))?;
            return Ok(BookWrite {
                before,
                venue_account_id: value,
                changed: false,
                verified_at: Some(verified_at.to_string()),
            });
        }
        return Ok(BookWrite {
            before,
            venue_account_id: value,
            changed: false,
            verified_at: None,
        });
    }

    // ⚠ The guard below is asked only of a SET. A CLEAR is the one act on this path that can never
    // reach a wrong broker: it removes an assertion rather than making one, and the state it leaves
    // — `NULL`, *not yet known* — is the state every migrated row is already in. Requiring
    // `--replace` to clear would put a second word in front of the move that takes a wrong book away.
    // `before.venue_account_id == value` was handled above, so this is a different SPELLING. It is
    // a different BOOK only when the two differ as `vike_model::BookId`s: a re-spelling of the same
    // book (`du1234567` -> `DU1234567`) re-points nothing, so it is written without `replace`.
    let same_book = value.as_deref().and_then(|v| vike_model::BookId::new(&before.venue, v))
        == before.book_id();
    if value.is_some()
        && let Some(current) = &before.venue_account_id
        && !replace
        && !same_book
    {
        return Err(refuse(StoreErrorKind::BookAlreadyKnown {
            id,
            venue: before.venue.clone(),
            tier: before.tier,
            current: current.clone(),
        }));
    }
    // No holder check: another row naming this number is not a refusal (decision 0274). Two ACTIVE
    // rows of one venue and tier on one book are the start's shared-book WARNING
    // (`vike_config::shared_books`), and N labelled rows on one venue account are ordinary.

    // ONE statement for both columns, so a stamped write cannot half-land: the timestamp says *this
    // book is what the venue answered at that instant*, and a commit carrying one without the other
    // would be a claim nobody made. `COALESCE(?3, last_verified_at)` leaves the column alone under
    // `BookSource::Operator` — a NULL parameter is *not my business*, never *clear it*.
    tx.execute(
        "UPDATE account SET venue_account_id = ?2, \
         last_verified_at = COALESCE(?3, last_verified_at) WHERE id = ?1",
        (id, &value, source.verified_at()),
    )
    .map_err(|e| StoreError::sql(path, e))?;
    tx.commit().map_err(|e| StoreError::sql(path, e))?;
    Ok(BookWrite {
        before,
        venue_account_id: value,
        changed: true,
        verified_at: source.verified_at().map(str::to_string),
    })
}
