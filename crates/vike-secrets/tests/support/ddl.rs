//! Legacy DDL planting — the schema shapes older releases shipped, derived from the frozen first-release batch.

// ---------------------------------------------------------------------------------------------
// §5.2 step 7 — the `venue_setting` shape a store held BEFORE `tier IS NULL` became `'any'`
// ---------------------------------------------------------------------------------------------
//
// ⚠ An ADDITION, not a move — the note at the top of this file ("moved verbatim") describes the
// `Fixture` above. This half exists because TWO test binaries need the pre-step-7 store and a
// second derivation of it would be a hand copy that rots: `tests/venue_setting_any_tier.rs` (step 7
// itself) and `tests/paper_tier.rs` (§4.4's rename, whose pre-rename store is necessarily ALSO a
// pre-step-7 one — the rename shipped first).

/// **The two partial indexes step 7 retires**, exactly as every shipped `DDL` up to and including
/// v0.1.34 spelled them — `CREATE … IF NOT EXISTS` and all, because the rollback test replays them
/// the way an OLDER binary's own batch would on its next write.
pub const LEGACY_TIER_INDEXES: &str = "\
CREATE UNIQUE INDEX IF NOT EXISTS venue_setting_one_per_tier
    ON venue_setting (venue, tier, field) WHERE tier IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS venue_setting_one_per_machine
    ON venue_setting (venue, field) WHERE tier IS NULL;
";

/// **The shipped `DDL` with the venue-links flip AND §5.2 step 7 reverted** — `venue_setting.tier`
/// nullable, its `CHECK` admitting NULL and not `'any'`, and the two partial indexes back in place
/// of the total `UNIQUE`. Byte-for-byte the `venue_setting` block v0.1.34 ships, which is the shape
/// the CI box's live store held on 2026-09-26.
///
/// ⚠ **The base is `vike_secrets::venue_links::pre_venue_link_ddl`, not the shipped `DDL`, since the
/// venue-links flip.** A pre-step-7 store is necessarily ALSO a pre-flip one — step 7 shipped first —
/// so every table in this batch carries the nullable `venue_id` and the text-keyed indexes that
/// v0.1.34 shipped, not only `venue_setting`. A base that kept the flipped `account` and
/// `venue_arming` would be a store
/// no binary ever wrote, and every raw `INSERT` a test plants on it without `venue_id` would be
/// refused before the test began. The store is aged in the order history made it: the flip undone
/// first, then step 7.
///
/// ⚠ **Derived rather than transcribed**, for the reason `tests/paper_tier.rs`' `aged_ddl` gives:
/// a test that plants its own guess proves the migration against a table nobody ever shipped. Each
/// replacement is asserted to match EXACTLY ONCE, so a respelling of the batch fails here rather
/// than leaving a fixture that quietly stopped aging anything. ⚠ The batch it derives from is the
/// FROZEN first-release one (`vike_secrets::venue_links::RELEASE_1_DDL`, through
/// `pre_venue_link_ddl`), not the shipped `DDL`: the venue-links plan's second release took the text
/// `venue` out of the shipped `account`, `credential` and `venue_setting`, and every shape this
/// file ages carries it.
pub fn pre_any_tier_ddl() -> String {
    const NEW_TIER: &str = "    tier     TEXT NOT NULL,\n";
    const OLD_TIER: &str = "    tier     TEXT,\n";
    const NEW_TAIL: &str = "    UNIQUE (venue, tier, field),\n    \
                            CHECK (tier IN ('any', 'paper', 'demo', 'live'))\n) STRICT;\n";
    let old_tail = format!(
        "    CHECK (tier IS NULL OR tier IN ('paper', 'demo', 'live'))\n) STRICT;\n\n\
         {LEGACY_TIER_INDEXES}"
    );

    let base = vike_secrets::venue_links::pre_venue_link_ddl();
    let ddl = base.as_str();
    for needle in [NEW_TIER, NEW_TAIL] {
        assert_eq!(
            ddl.matches(needle).count(),
            1,
            "the pre-flip batch (`pre_venue_link_ddl`) must spell step 7's `venue_setting` exactly \
             once as {needle:?} — if this fails the fixture below is no longer aging anything and \
             every test built on it would pass vacuously"
        );
    }
    let aged = ddl.replacen(NEW_TIER, OLD_TIER, 1).replacen(NEW_TAIL, &old_tail, 1);
    assert!(
        aged.contains("tier IS NULL OR tier IN") && !aged.contains("'any'"),
        "the aged batch must admit a NULL tier and must not know the word `'any'`"
    );
    aged
}

// ---------------------------------------------------------------------------------------------
// The venue-links plan's first release — older shapes than the one it carries
// ---------------------------------------------------------------------------------------------
//
// ⚠ The batch the release before the flip shipped is
// `vike_secrets::venue_links::pre_venue_link_ddl`, behind `test-support`: it lived here until a test
// in another crate needed it, and a test directory is visible to no other crate. It moved rather
// than being copied, so there is one derivation.

/// **The first release's frozen `DDL` with every venue link taken out**:
/// `vike_secrets::venue_links::pre_venue_link_ddl` with every `venue_id` line gone. ⚠ It is NOT the
/// batch as it stood before stage 2: it still carries `AUTOINCREMENT`, the `'paper'` tier and step
/// 7's `venue_setting`, which all came later. What it
/// isolates is the one thing a pre-stage-2 store lacks that the venue-links pass cares about, the
/// column; a test that needs the older shapes too ages them on top, as [`pre_any_tier_ddl`] does
/// for step 7. `ALTER TABLE … DROP COLUMN` cannot produce it any more, because SQLite refuses to
/// drop a column an index or a `UNIQUE` names, and since the venue-links flip both do. So the batch
/// is derived instead, with each needle asserted to match exactly once. The `venue` table itself
/// stays; a caller aging past it drops it.
///
/// ⚠ Here rather than in one test file because two need it: the venue-links plan's own
/// pre-stage-2 test, and `tests/database_migration.rs`' `planted_schema_2_without_venue`, which
/// built the same store by `DROP COLUMN` until the flip made that impossible.
pub fn pre_stage_2_ddl() -> String {
    let mut ddl = vike_secrets::venue_links::pre_venue_link_ddl();
    for needle in [
        "    venue_id         INTEGER REFERENCES venue(id),\n",
        "    venue_id      INTEGER REFERENCES venue(id),\n",
        "    venue_id INTEGER REFERENCES venue(id),\n    tier     TEXT NOT NULL,\n",
        "    venue_id INTEGER REFERENCES venue(id),\n    label    TEXT,\n",
    ] {
        assert_eq!(ddl.matches(needle).count(), 1, "the pre-flip batch spells {needle:?} once");
        let kept = needle.split_once('\n').map(|(_, rest)| rest).unwrap_or("");
        ddl = ddl.replacen(needle, kept, 1);
    }
    assert!(!ddl.contains("venue_id"), "no `venue_id` may survive: {ddl}");
    ddl
}
