//! The profile tables' DDL: the schema template, the function that renders it, and the table list.

use super::*;

// ---------------------------------------------------------------------------------------------
// The schema
// ---------------------------------------------------------------------------------------------

/// **The profile half of the settings store** — §4.4 `profile`, §4.5 `mount`, §4.6 `mount_param`
/// and §4.8 `profile_setting`, plus the two columns argued in this module's doc.
///
/// # The deviations from the printed DDL, each argued where it is made
///
/// ⚠ This heading counted them ("the three") while five were numbered below it, and the count was
/// short from the day the fourth landed. The numbering IS the list; do not restate its length here.
///
/// **1. `profile.active`, with a partial unique index per KIND.** The owner ruled the row wins; a
/// column is what a row needs to say so. `WHERE active = 1` is what makes "at most one live daemon
/// profile" a property of the SCHEMA rather than of whoever wrote last — and it is per `kind`,
/// because a daemon profile and a run profile are selected independently today (MEASURED: the
/// shipped unit's `ExecStart --config` chooses one and `VIKE_RUN_PROFILE` the other) and collapsing
/// them into one active row would invent a coupling the file world does not have.
///
/// **2. `mount.is_primary`, with a partial unique index per PROFILE.** See
/// [`StoredProfile::primary`] for what the column means and what its absence means.
///
/// **3. `kind` keeps §4.4's three-word CHECK, and all three words are READ.** ⚠ **This deviation
/// said the opposite until now — *"`recorder` is refused in RUST … [`ProfileKind`] is the refusal,
/// and it is total"* — and it had been false since 2026-09-16**, when the owner overruled 0057's
/// NO and [`ProfileKind::parse`]'s refusal arm was deleted. What survives is the reason the CHECK
/// was never narrowed to two words: doing so would have made the schema disagree with the spec it
/// is built from over a POLICY rather than a shape, and honouring 0057's flip condition (the
/// deploy-layout question being answered) would then have cost a schema change on every store. The
/// condition fired, the flip cost nothing, and the foresight is why — so the deviation is now
/// simply that this schema admits a word the printed DDL's prose argued about.
///
/// **4. `mount.asset_class`, NOT NULL, with a `CHECK` whose words are a PARAMETER.** Phase 5 of
/// `docs/decisions/0061-an-instrument-names-its-kind.md`, ORDERED by the owner: *"we need to add
/// type if it is spot or perp or option or anything else bcz it will help us in future"* — a claim
/// about what a mount should MEAN, not about any current bug. The vocabulary is
/// `vike_model::AssetClass`, unchanged and unwidened.
///
/// **Required rather than nullable, also his call**, and the reason is measurable: nothing in this
/// tree writes a profile row yet (`crates/vike-ops/tests/settings/profile_writer_gate.rs`'s `WRITER_CALLERS`
/// is EMPTY, and that is its own stated measurement), so no `mount` table exists on any box and the
/// migration that makes the column mandatory costs one value in one file. A nullable column would
/// instead make 0061's *"a missing claim becomes a legal value"* hazard permanent for this seam.
///
/// ⚠ **THE DOOR THIS PARAGRAPH SAID WAS SHUT IS OPEN, AND THE PARAMETER STAYS ANYWAY.** The word
/// list is a PARAMETER, and the reason has now expired twice. It was first spelled
/// as a LAYER one (this crate 15, `vike-catalog` 20); the taxonomy then moved to `vike-model` at
/// layer 10, which the layer rule permits, and the paragraph was amended to rest on the remaining
/// floor instead — *"`vike-secrets` is a **zero-`vike-*`-dependency leaf** — by design and by gate
/// (`crates/vike-boot/tests/dependency_floor.rs`) — so this module cannot name `AssetClass` from
/// any crate at any layer … so do not read the move as having opened this door."*
///
/// **The move DID open it, and the tier itself now says so.** Tier 15's rule is *at most the
/// floor* — nothing above rank 10 — not *no `vike-*` dependency at all*, and
/// `crates/vike-ops/tests/arch/layer_gate/tiers.rs`'s
/// `every_tier_15_crate_names_nothing_above_the_vocabulary` is where that is machine-checked. The
/// taxonomy is AT rank 10, which is inside the floor, so `vike_model::AssetClass::SQL_WORDS` is
/// nameable from this module on the RANK alone; the manifest then made it nameable in fact
/// (`docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md`, accepted
/// 2026-09-20). The amendment's warning points the wrong way on both counts.
///
/// **RULED 2026-09-26: the parameter STAYS, and NOT because this module cannot name the enum.**
/// 0072 admitted an edge and ruled nothing about seams, so the question it stranded here is
/// answered rather than left for somebody to re-derive from the same expired premise. Three
/// reasons, none of them a dependency claim:
///
///   * **There is nothing to collapse TO.** 0072's cure was for a DUPLICATION — two copies of one
///     project walk, held equal by a test in a third crate — and *"one definition needs no pin"*
///     was the whole of its argument. This is not that shape: the list is PASSED, not copied, so a
///     collapse deletes no second spelling and retires no pin that a duplication was paying for.
///     The account-label constants that fell in the same sweep WERE duplicated constants; an
///     injected decision does not generalise from them, and this doc used to say so.
///   * **The refusal below is reachable only because the words arrive as DATA.**
///     [`ProfileError::UnrenderableVocabulary`] is the whole of what stands between a caller's
///     string and a `CHECK` clause, and this function is the one site in this tree that
///     interpolates a string into DDL. Named inline, its loop would validate a `&'static` constant
///     that no test could make fail — the module would TRUST its vocabulary where today it CHECKS
///     it, which is the posture this block's closing sentence rejects in as many words
///     (*"rather than trusting that"*).
///   * **The collapse is a six-crate signature change bought for one fewer pass-through
///     argument.** The vocabulary threads through four entry points here,
///     `crates/vike-cli/src/cmd/config/mirror_profile.rs`'s `write` and its recorder twin, and
///     `crates/vike-tradehub/src/profile_rows.rs`'s `mount_asset_class_vocabulary` — a public
///     accessor it would retire, along with `crates/vike-ops/tests/settings/profile_writer_gate.rs`'s fifth
///     instruction and the cross-crate pin
///     `crates/vike-tradehub/tests/daemon/profile_rows.rs`'s
///     `mount_asset_class_vocabulary_is_the_enums_own`.
///
/// ⚠ **The argument AGAINST, recorded because it is real and this ruling does not dissolve it.**
/// The parameter accepts any bare-alphanumeric list, so a caller CAN bake a `CHECK` that refuses
/// classes the enum has, and `CREATE TABLE IF NOT EXISTS` never repairs one. That every production
/// caller hands over the enum's own words is held by prose and by a gate row rather than BY
/// CONSTRUCTION, and this tree prefers construction wherever it can have it. **What would reopen
/// this: a second production supplier of the vocabulary.** There is exactly one today, and
/// `WRITER_CALLERS` is what keeps it that way; the day a caller spells a list of its own, the
/// argument above has already lost and the collapse is the cure.
///
/// 0061 refuses a second hand-written list outright (*"two lists —
/// one in code, one in SQL — would be the fifth partial encoding this record refuses everywhere
/// else"*). So this module spells NO asset-class word anywhere: the caller hands over
/// `vike_model::AssetClass::SQL_WORDS`, which is itself generated from the enum's single
/// declaration, and the `CHECK` clause in the database is that declaration rendered. Interpolating
/// caller strings into SQL is safe for exactly the reason [`ProfileKind::sql_word`] gives for its
/// own — they are `&'static str`s off a closed enum, never operator input —
/// and [`profile_ddl`] REFUSES anything that is not a bare alphanumeric word rather than trusting
/// that.
///
/// **5. `recorder` + `subscription`, added 2026-09-16 when the owner overruled 0057's NO.** Six
/// decisions, each of which had an available alternative:
///
///   * **TYPED COLUMNS, not a `profile_setting` bag.** `[maintenance]` and `[alerting]` are five
///     and three named keys, fixed and small. As columns, SQLite refuses an `INSERT` naming a key
///     that does not exist — on EVERY write path including a hand `INSERT`, with no Rust
///     involved — which is `deny_unknown_fields` made STRONGER by the move rather than lost to it.
///     A `(path, value)` bag would have made a typo'd path a row that reads as nothing.
///   * **EVERY KNOB NULLABLE, and NULL means ABSENT — never "the default".** The defaults live in
///     `vike_recorder::config`'s `Default` impls and stay there; a column that stored the resolved
///     default would make a re-rendered document differ from the file it came from, and the
///     migration's whole fence is that the two are EQUAL. `store` is the one NOT NULL column,
///     because the file requires it too.
///   * **`CHECK` for the closed VOCABULARY only** (`backfill IN ('venue','archive','off')`), never
///     for a numeric bound. `min_parts >= 2`, `target_mb > 0`, `max_merge_rows > 0` and
///     `retention_days > 0` are all CONDITIONAL on `interval_secs > 0`, which a row-local `CHECK`
///     cannot express — and this module's own rule forbids restating a numeric bound in SQL
///     regardless. They stay in `vike_recorder::config::RecorderProfile::validate`, which a
///     row-loaded profile reaches through the same `from_toml` a file-loaded one does.
///   * **`venue` gets NO `CHECK`.** The set a build can record is a per-BUILD fact
///     (`crates/vike-datahub/src/recording.rs`'s `supported`, `#[cfg(feature = …)]`) while this
///     store is one file shared by every binary on the box, so a `CHECK` would refuse at write time
///     a row a differently-featured build could record. The Rust refusal in
///     `crates/vike-datahub/src/recorder.rs`'s `load_and_check_profile` is the authority and is
///     unchanged.
///   * **`symbols` and `alert_webhooks` hold a TOML ARRAY RENDERING in one column**, the same
///     idiom `mount_param.value` and `profile_setting.value` already use for a composite. A child
///     table was the normalized alternative and buys nothing here: the whole list shares one
///     `backfill`, `FamilyAndSymbols` reasons about the subscription as ONE thing, and a child
///     table cannot be reached by the exclusivity check anyway.
///   * **`subscription.ord` EXISTS AND IS NOT `mount.ord`.** `mount`'s ordinal is an ATTRIBUTION
///     key (`coid_mount`, the `{idx}|` strategy-tag prefix), so reordering relabels a live ledger.
///     This one is a row identity that also preserves authoring order, which
///     `crates/vike-datahub/src/recorder.rs`'s `record` uses only to build feeds in the order the
///     operator wrote them — cosmetic, and stated here so nobody infers the other meaning. The
///     `DuplicateFamily` refusal rides the partial unique index instead, which is where a
///     uniqueness claim belongs; the `family`/`symbols` exclusivity stays in Rust, because a
///     `CHECK` over `(family IS NULL) != (symbols IS NULL)` would forbid the both-absent case
///     with the WRONG message (`ProfileError::Empty` names what it would record: nothing).
///
/// `ON DELETE CASCADE` throughout, and `PRAGMA foreign_keys` is set and VERIFIED by `crate::db`'s
/// `open_for_write` — without it every `REFERENCES` here would enforce nothing, which is the trap
/// that module's doc already records for the credential family.
///
/// # Errors
///
/// [`ProfileError::UnrenderableVocabulary`] when a supplied word is empty or is not bare ASCII
/// alphanumerics.
pub fn profile_ddl(asset_class_words: &[&str]) -> Result<String, ProfileError> {
    if asset_class_words.is_empty() {
        return Err(ProfileError::UnrenderableVocabulary { word: String::new() });
    }
    for word in asset_class_words {
        if word.is_empty() || !word.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(ProfileError::UnrenderableVocabulary { word: (*word).to_string() });
        }
    }
    let words = asset_class_words.iter().map(|w| format!("'{w}'")).collect::<Vec<_>>().join(", ");
    Ok(PROFILE_DDL_TEMPLATE.replace(ASSET_CLASS_WORDS_PLACEHOLDER, &words))
}

/// The token [`profile_ddl`] substitutes the caller's vocabulary for. Never reaches a database.
pub(super) const ASSET_CLASS_WORDS_PLACEHOLDER: &str = "{{ASSET_CLASS_WORDS}}";

/// The schema with the asset-class vocabulary left as a hole — see [`profile_ddl`], which is the
/// only thing that may render it. Private on purpose: a caller that executed this verbatim would
/// create a `mount` table whose `CHECK` refuses every word there is.
const PROFILE_DDL_TEMPLATE: &str = "\
CREATE TABLE IF NOT EXISTS profile (
    name        TEXT PRIMARY KEY NOT NULL,
    kind        TEXT NOT NULL,
    active      INTEGER NOT NULL DEFAULT 0,
    note        TEXT,
    updated_utc INTEGER,
    updated_by  TEXT,
    CHECK (kind IN ('daemon', 'run', 'recorder')),
    CHECK (active IN (0, 1))
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS profile_one_active_per_kind
    ON profile (kind) WHERE active = 1;

CREATE TABLE IF NOT EXISTS mount (
    profile          TEXT NOT NULL REFERENCES profile(name) ON DELETE CASCADE,
    ord              INTEGER NOT NULL,
    is_primary       INTEGER NOT NULL DEFAULT 0,
    venue            TEXT NOT NULL,
    asset_class      TEXT NOT NULL,
    symbol           TEXT,
    token_id         TEXT,
    interval         TEXT,
    interval_ms      INTEGER,
    resolution_ts_ms INTEGER,
    qty              REAL,
    half_spread      REAL,
    tick_size        REAL,
    seed_cash        REAL,
    data_only        INTEGER,
    account          TEXT,
    strategy_name    TEXT,
    strategy_rhai    TEXT,
    note             TEXT,
    PRIMARY KEY (profile, ord),
    CHECK ((symbol IS NULL) != (token_id IS NULL)),
    CHECK (is_primary IN (0, 1)),
    CHECK (data_only IS NULL OR data_only IN (0, 1)),
    CHECK (asset_class IN ({{ASSET_CLASS_WORDS}}))
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS mount_one_primary_per_profile
    ON mount (profile) WHERE is_primary = 1;

CREATE TABLE IF NOT EXISTS mount_param (
    profile   TEXT NOT NULL,
    mount_ord INTEGER NOT NULL,
    key       TEXT NOT NULL,
    value     TEXT NOT NULL,
    PRIMARY KEY (profile, mount_ord, key),
    FOREIGN KEY (profile, mount_ord) REFERENCES mount(profile, ord) ON DELETE CASCADE
) STRICT;

CREATE TABLE IF NOT EXISTS profile_setting (
    profile     TEXT NOT NULL REFERENCES profile(name) ON DELETE CASCADE,
    path        TEXT NOT NULL,
    value       TEXT NOT NULL,
    note        TEXT,
    updated_utc INTEGER,
    updated_by  TEXT,
    PRIMARY KEY (profile, path)
) STRICT;

CREATE TABLE IF NOT EXISTS recorder (
    profile             TEXT PRIMARY KEY NOT NULL REFERENCES profile(name) ON DELETE CASCADE,
    store               TEXT NOT NULL,
    interval_secs       INTEGER,
    min_parts           INTEGER,
    target_mb           INTEGER,
    max_merge_rows      INTEGER,
    retention_days      INTEGER,
    alert_webhooks      TEXT,
    alert_repeat_secs   INTEGER,
    alert_series_prefix TEXT,
    note                TEXT
) STRICT;

CREATE TABLE IF NOT EXISTS subscription (
    profile  TEXT NOT NULL REFERENCES profile(name) ON DELETE CASCADE,
    ord      INTEGER NOT NULL,
    venue    TEXT NOT NULL,
    family   TEXT,
    symbols  TEXT,
    backfill TEXT,
    note     TEXT,
    PRIMARY KEY (profile, ord),
    CHECK (backfill IS NULL OR backfill IN ('venue', 'archive', 'off'))
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS subscription_one_family_per_venue
    ON subscription (profile, venue, family) WHERE family IS NOT NULL;
";

/// The tables [`profile_ddl`] creates. Used by the presence probe so the read path can tell "this
/// store predates Phase 3" from "this store has an empty profile table", and by the tests so a new
/// table cannot be added without the presence probe learning about it.
pub const PROFILE_TABLES: [&str; 6] =
    ["profile", "mount", "mount_param", "profile_setting", "recorder", "subscription"];
