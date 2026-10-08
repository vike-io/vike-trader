//! **No NULL in this store's schema may be the SOLE discriminator of a row's KIND** — owner
//! ruling 2 of `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md`: *"A design
//! where a NULL in a column decides what KIND of row it is is refused on principle, even where it
//! is unambiguous."* §7 item 2 of that spec asks for this gate in as many words, and calls it *"the
//! gate whose absence let §2.1's drift land"*.
//!
//! The shape it is about was live in two places and the spec measures both. §2.2, quoting
//! `crates/vike-config/src/mirror.rs`'s own wart comment beside `rows_from_loaded`: on
//! `venue_arming`, *"`label IS NULL` already means both 'the venue's ceiling' and 'the unlabelled
//! account's'"*. §2.6: `venue_setting` *"carries the identical NULL-discriminator shape"* —
//! `tier IS NULL` is the machine-scoped row, `tier NOT NULL` the tier-scoped one.
//!
//! ⚠ **§2.6's half is PAID (§5.2 step 7, 2026-09-26)** — `venue_setting.tier` is `NOT NULL`, the
//! machine scope is the stored word `'any'`, and one total `UNIQUE` replaced the two partial
//! indexes, so this gate observes nothing on that table any more. This paragraph said *"is live in
//! two places"* until then; `venue_arming.label` is the one left.
//!
//! # What is DERIVED, and why the spec's literal sentence was narrowed
//!
//! §7 item 2 spells the rule as *"fails on a partial index whose `WHERE` clause tests a column for
//! NULL"*. Taken literally that condemns every partial index in [`DDL`] — seven when this was
//! written (it said *"all seven"*), five since §5.2 step 7 retired two — three of which are not the
//! defect at all: `credential`'s two live-row indexes test `superseded_at IS NULL` and
//! `account_one_account_per_book` tests `venue_account_id IS NOT NULL`. Those are FILTERS — they
//! select a SUBSET of rows to constrain and NOTHING constrains the complement, neither a second
//! index nor the table's own natural key, so no row's KIND is being decided. Pinning them as debt
//! would be a lie in the opposite direction from the one this file exists to prevent, and the pin
//! would never shrink.
//!
//! ⚠ **That narrowing is the SPEC's, not this file's.** §6 exempts one of the three BY NAME:
//! *"`superseded_at IS NULL` remains a NULL that means 'not yet'. It does not discriminate a KIND
//! of row, and the timestamp is genuinely useful, so ruling 2 does not reach it."* The third,
//! `account_one_account_per_book`'s `venue_account_id IS NOT NULL`, the spec does not rule on —
//! its `Filter` is this gate's own derivation, and [`NULL_PREDICATE_PIN`]'s row says so.
//!
//! So the KIND verdict is derived from the DDL rather than hand-assigned, by the property that
//! separates the two — **the nullness of this column selects which uniqueness rule applies** —
//! which the schema can say along either of [`Route`]'s two paths. Today they derive
//! `venue_arming.label` — the one the spec names that is still standing — from the shipped schema
//! rather than from the spec's prose. (They derived `venue_setting.tier` beside it until §5.2
//! step 7 took that table's partial indexes away.)
//!
//! Every NULL-testing index is still OBSERVED and must still be PINNED with its verdict and its
//! reason, so the literal rule survives as the classification duty: a new one reddens
//! [`every_null_predicate_is_classified`] until its author writes down which of the two it is.
//! That is the repo's per-venue-table idiom — a named row proves the case was classified rather
//! than forgotten.
//!
//! # ⚠ This lands GREEN with the debt pinned, NOT red, and the instruction to the contrary is
//! self-defeating
//!
//! §9's stage 1 schedules this gate *"written against the CURRENT schema, failing … they document
//! the debt"*. A red gate cannot be landed at all — `crates/vike-secrets/tests/gates/store_link.rs`
//! says so in its own words for the sibling gate, and that one shipped as a ratchet for the same
//! reason. The debt is documented by [`NULL_PREDICATE_PIN`]'s `KindDiscriminator` rows, which name
//! it, and by [`the_pin_has_no_stale_rows`], which turns removing one into a required edit here.
//!
//! ⚠ **No count of what a later stage removes is written here, and one WAS — wrongly.** This
//! paragraph said the `venue_arming` drop "shrinks this array by four rows"; it is the rows naming
//! `venue_arming_one_per_venue` and `venue_arming_one_per_account`, and a `venue_setting` fix
//! takes the rows naming `venue_setting_one_per_tier` and `venue_setting_one_per_machine`. Nothing
//! gates a derived count stated in prose, which is why this repo turns counts into declared array
//! lengths — so the retirements are NAMED instead, and [`the_pin_has_no_stale_rows`] prints
//! exactly which lines to delete when the day comes. ⚠ **The `venue_setting` day came first**
//! (§5.2 step 7, 2026-09-26) — against this file's own `venue_setting_one_per_machine` row, which
//! predicted *"this pair outlives the pair above"* — and it took exactly the two rows named here.
//!
//! # A sibling gate pins the same indexes
//!
//! `crates/vike-secrets/tests/settings_store_ddl_gate.rs` compares the shipped `DDL` against §3's
//! printed schema (§7 item 5), and its `SPEC_DRIFT_PIN` carries `venue_arming`'s two indexes as
//! DRIFT rather than as ruling-2 debt, each under an `index:` row of its own. The two gates ask
//! different questions of the same lines and neither subsumes the other: that one goes green when
//! the DDL matches the SPEC, this one when the schema stops letting a NULL decide a row's kind.
//! **Whoever retires one of these indexes has to edit both files**, and the failure message here
//! will not mention the other — hence this paragraph.
//!
//! ⚠ **This heading said "the same four" and the paragraph carried two errors until §5.2 step 7
//! (2026-09-26)**: it counted `venue_setting`'s two partial indexes in (step 7 retired them from
//! both files in one edit, as this paragraph asks), and it said `venue_arming`'s two were *"folded
//! into its `table:venue_arming` row"* — they never were; that row's own text says its indexes are
//! *"pinned by name below"*.
//!
//! ⚠ That pin also showed §3's plan landing exactly the shape [`Route::NaturalKey`] exists for: its
//! `venue_setting.unique:venue_id,tier,field` row said *"§3 replaces the two partial indexes below
//! with ONE total `UNIQUE`"*. Step 7 did, and with NO partial index left beside it — so this gate
//! observes nothing on that table, which is the win. A total `UNIQUE` with ONE partial index still
//! beside it would have been the defect, and only that Route can see it; step 7's own migration
//! refuses to finish while either retired index is still on the table for the same reason.
//!
//! # Declared residuals
//!
//! * **A SINGLE-direction NULL index whose column reaches NEITHER of [`Route`]'s two paths is
//!   classified `Filter`.** One route was added after review measured the gap the other left: a
//!   table-level `UNIQUE (…)` is a uniqueness rule that no index `WHERE` mentions, so §3's
//!   replacement shipping `UNIQUE (venue_id, account_id)` beside a lone
//!   `CREATE UNIQUE INDEX … WHERE account_id IS NULL` would have re-landed ruling 2's defect inside
//!   the redesign this gate exists to guard, deriving `Filter` while the author pinned `Filter` and
//!   [`every_pinned_verdict_matches_the_schema`] AGREED. [`Route::NaturalKey`] closes that. What
//!   remains uncovered is a partition whose OTHER half is enforced by nothing declarative at all —
//!   application code, or a uniqueness rule nobody wrote down. The author still has to write a pin
//!   row and argue it, so it is not invisible; the gate will not say the word.
//! * **[`DDL`] is the whole scope of the pin.** `crates/vike-secrets/src/profile_store/ddl.rs`'s
//!   `profile_ddl` renders a SECOND schema in this crate.
//!   [`the_profile_store_schema_carries_no_kind_discriminator`] runs the same derivation over it —
//!   measured, not assumed away — but does not pin its filters, so that schema is covered for
//!   ruling-2 offences only.
//! * **A `CHECK` constraint is not read.** `mount`'s `CHECK ((symbol IS NULL) != (token_id IS
//!   NULL))` is a mutual-exclusion rule rather than a uniqueness partition; this gate reads index
//!   `WHERE` clauses, which is the surface §7 item 2 names.

mod gate;
mod pin;
