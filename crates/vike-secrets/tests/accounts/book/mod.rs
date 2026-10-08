//! **The `account` table gets a WRITER for one column** — `venue_account_id`, the BOOK as the venue
//! names it — and every test here is about a way that writer could be wrong while every other test
//! in this crate stayed green.
//!
//! `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §4.5 records this column as one
//! *nothing in this tree writes*, and names two writers it should eventually have: the migration's
//! FOLD of the ten stored keys that already are the book (§11 step 3) and the venue's own
//! HANDSHAKE (§12). `vike_secrets::set_venue_account_id` is NEITHER. It is the third source, for
//! the one case those two cannot reach: a book that is in no key and derivable from nothing in the
//! store.
//!
//! ⚠ **That case is dukascopy, and it is the whole reason this file exists.** After a migration the
//! two dukascopy demo accounts are `(dukascopy, demo, label = NULL)` twice over — `UNIQUE (venue,
//! tier, label)` does not separate them, because NULLs are distinct in SQLite — so `id` is the only
//! handle that tells them apart, and nothing records which row is which BOOK. They are two legal
//! entities (Dukascopy Bank SA and Dukascopy Europe IBS AS), so a book written to the wrong row
//! routes orders to the wrong BROKER.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`the_two_dukascopy_rows_learn_different_books_and_stay_two_rows`] | the premise: two accounts a name cannot separate, separated and then IDENTIFIED |
//! | [`writing_the_same_book_twice_writes_nothing_and_says_so`] | a re-run reported as a change, or refused as a conflict with itself |
//! | [`a_row_that_names_a_different_book_is_refused_until_replace_says_so`] | ⚠ the one that matters most: a mistyped id silently re-pointing an armed account at another broker |
//! | [`two_active_accounts_of_one_venue_may_not_name_one_book`] | ruling 11 (§8) arriving as an opaque `UNIQUE constraint failed`, or not at all |
//! | [`an_id_no_row_carries_is_refused_and_creates_no_account`] | a typo inventing an account, and a book landing on one nobody has |
//! | [`a_file_store_is_refused_and_no_database_is_created`] | the per-KEY fallback `Backend` forbids — or worse, a MINTED database that retires every credential on the box |
//! | [`a_schema_1_store_says_it_predates_the_account_table`] | an unmigrated box handed the engine's own `no such table` as if the store had malfunctioned |
//! | [`a_malformed_book_is_refused_and_the_refusal_echoes_no_token`] | a pasted secret quoted back into the terminal by the refusal that was meant to protect it |
//! | [`the_write_touches_no_credential_no_file_and_no_other_column`] | a writer that "tidied" a neighbouring column, a row, or the credential file |
//! | [`the_normalizer_trims_the_paste_and_refuses_everything_else`] | `"4100017 "` and `"4100017"` colliding — or NOT colliding — by an invisible byte |
//! | [`an_invisible_character_is_trimmed_at_the_edge_and_refused_in_the_middle`] | ⚠ a pasted byte-order mark STORED: two books identical on every screen, different to the index |
//! | [`a_swapped_pair_is_repaired_by_clearing_one_row_first`] | ⚠ a pair written the wrong way round with no repair in the tree — refused in BOTH directions, forever |
//! | [`a_clear_is_idempotent_and_local`] | a clear that needed permission it cannot need, or that reached a neighbouring row |
//! | [`a_clear_of_a_missing_row_is_refused_and_creates_nothing`] | the create this writer may never perform, reached through the one path that takes no value |
//! | [`the_key_names_are_what_tell_the_two_identical_rows_apart`] | ⚠ the premise of the whole verb: two rows an operator CANNOT choose between, and a writer addressed by a handle nobody can resolve |
//! | [`a_file_store_cannot_be_keyed_and_says_so_rather_than_answering_empty`] | *no keys* and *no table* merged into one blank column |
//!
//! ⚠ **Every value in this file is FICTIONAL, the two dukascopy books included.** An earlier draft
//! carried the owner's real measured numbers and his two real demo LOGIN ids, on the stated grounds
//! that a test is not published. **That ground is false and was measured:**
//! `scripts/publish_mirror.sh`'s `ALLOW` carries `crates` wholesale and its exclusions reach
//! `crates/vike-ops/tests` plus a handful of named files — this path is in neither, so this file
//! ships to the public mirror like any source file. The login half was the worse of the two: that
//! venue's password convention is the login's own last five characters, so publishing a login
//! publishes its password.
//!
//! Nothing here needs the real values. What the test proves is that the STORE can hold two
//! DIFFERENT books on two rows no name can separate — a property of any two distinct strings. The
//! real mapping lives in `docs/superpowers/specs/2026-09-14-the-credential-schema.md`, and `docs/`
//! is never published.

mod clear_and_swap;
mod fixture;
mod handshake;
mod no_credential_leak;
mod premise_and_refusals;
