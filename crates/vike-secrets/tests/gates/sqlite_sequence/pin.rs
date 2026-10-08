//! The pin: which tables of the plane carry an `AUTOINCREMENT` high-water mark, and why.

use std::collections::BTreeMap;

// -------------------------------------------------------------------------------------------
// The pin
// -------------------------------------------------------------------------------------------

/// Whether a table of this plane has an `AUTOINCREMENT` high-water mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Sequence {
    /// The table's own `id` declares `AUTOINCREMENT`, so SQLite keeps a `sqlite_sequence` row for
    /// it and a rebuild can LOSE that row.
    Armed,
    /// **The debt.** It does not, so SQLite hands a new row `max(rowid) + 1` and a freed id comes
    /// straight back out. A row carrying this word names the stage that retires it.
    Owed,
}

impl std::fmt::Display for Sequence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Sequence::Armed => "Armed",
            Sequence::Owed => "Owed",
        })
    }
}

/// **Every table this gate has a verdict about**, as `(table, verdict, why)`.
///
/// ⚠ **The declared length is the COUNT** — no prose anywhere, this file included, may restate it.
///
/// This is deliberately NOT every table in [`DDL`]. ⚠ **It was one row short of every ARMED table
/// until stage 4 landed, and the sentence that stood here said why: §4.1's identity ruling names
/// `account` alone, so the plane's other id-bearing tables carried no row.** Ruling 6 then gave
/// every one of them the same shape — *"One uniform table shape, so no per-table judgment is
/// required"* — and [`every_armed_table_is_classified`] turned that into rows here, which is the
/// growth this gate was built to force rather than a widening somebody chose.
///
/// Two tables in [`DDL`] are still absent, and neither escapes by accident: `settings_adoption` is
/// ruling 6's one STATED exception (*"a singleton whose `id` is a seal rather than a surrogate"*)
/// and `venue_arming` is a table §3 spells DELETED — neither declares `AUTOINCREMENT`, so neither
/// has a mark to keep, and the day either one gains one this gate reddens naming it.
pub(super) const SEQUENCE_PIN: [(&str, Sequence, &str); 7] = [
    (
        "account",
        Sequence::Armed,
        "spec §2.4 and §4.1 — the ruled identity, and the one table §7 item 4 names. ⚠ ARMED by \
         §9's stage 4 on 2026-09-23, which is what made every carry in this crate load-bearing: \
         `crate::schema::rebuild_table_from_ddl` is the one rebuild procedure and both migrations \
         that call it now touch an armed table. \
         `the_paper_tier_rebuild_does_not_rewind_the_account_marks` is the behaviour assertion \
         that fails if its mark carry is removed, and \
         `section_7_item_4_remove_the_top_account_rebuild_and_create_again` is §7 item 4 proper, \
         now asserting NON-reuse",
    ),
    (
        "venue",
        Sequence::Armed,
        "armed since stage 2 seeded it, and the first table a rebuild could rewind — \
         `account.venue_id`, `credential.venue_id`, `venue_arming.venue_id` and \
         `venue_setting.venue_id` all REFERENCE it, so a reused venue id re-points live rows at a \
         different venue rather than merely confusing an operator's note",
    ),
    (
        "credential",
        Sequence::Armed,
        "ruling 6's uniform shape, armed by stage 4. Its rows hold the live venue keys and \
         `credential_one_live_name` is what tells one account's key set from another's, so an id \
         handed out twice names two different SECRETS across time; nothing in the tree stores a \
         credential id, which is why this is a shape guarantee rather than an addressing one",
    ),
    (
        "venue_setting",
        Sequence::Armed,
        "ruling 6's uniform shape, armed by stage 4. ⚠ It is the SECOND table \
         `crate::schema::migrate_sim_tier_to_paper` rebuilds, so its mark carry is load-bearing \
         for exactly the same reason `account`'s is",
    ),
    (
        "setting",
        Sequence::Armed,
        "ruling 6's uniform shape, armed by stage 4. Addressed by `UNIQUE (section, key)` and \
         never by id, so what the mark protects here is the uniform shape itself",
    ),
    (
        "profile_risk",
        Sequence::Armed,
        "ruling 6's uniform shape, armed by stage 4. Addressed by `UNIQUE (profile, key)`, same \
         as `setting` above",
    ),
    (
        "node_key",
        Sequence::Armed,
        "ruling 6's uniform shape, armed by stage 4 — and the one table that had NO surrogate id \
         at all before it (`name TEXT PRIMARY KEY`). Its `name` keeps a `UNIQUE`, which is what \
         `crates/vike-secrets/src/db.rs`'s `ON CONFLICT(name) DO UPDATE` upsert resolves against, \
         so 0051's pair is still addressed by NAME and the id is the shape, not the address",
    ),
];

pub(super) const GROWTH_GUIDANCE: &str = "\
A table in the shipped `DDL` now declares `AUTOINCREMENT` and nothing here says so. Add its row:

  * `Sequence::Armed` — and then answer the question this file exists to ask: every rebuild of that
    table must carry its `sqlite_sequence` mark across, because a rebuild that replays surviving
    rows after the top one was removed rewinds the mark. `rebuild_preserving_ids` in this file
    measures that ONE property — the mark carry — which every real rebuild in this crate also
    performs; its own doc says plainly where its rename-aside shape otherwise diverges from theirs.
  * `Sequence::Owed` — the table has no mark and its ids are reused. Name the stage that arms it.";

pub(super) fn pinned() -> BTreeMap<String, Sequence> {
    SEQUENCE_PIN.iter().map(|(table, verdict, _)| ((*table).to_string(), *verdict)).collect()
}
