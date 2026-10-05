use super::*;

/// **The anti-vacuity guard for [`required_columns_of`]**, and it is the only thing standing
/// between [`rebuild_table_from_ddl`] and the one failure it must never cause: a rebuild of a
/// schema-1 `credential` (`name TEXT PRIMARY KEY, value TEXT`) into the shipped shape, whose
/// `field NOT NULL` would be handed NULL and abort an operator's whole credential write.
///
/// A parser that quietly answered EMPTY would make that guard a no-op while every other test
/// here stayed green, so the answer is asserted by NAME for the table it matters on, and
/// asserted non-empty for every table in the batch.
#[test]
fn the_required_column_derivation_names_what_a_rebuild_must_be_able_to_fill() {
    let credential = required_columns_of("credential");
    assert!(
        credential.iter().any(|c| c == "field"),
        "`credential.field` is `TEXT NOT NULL` with no DEFAULT, so a schema-1 table (which has \
             no such column) must be REFUSED a rebuild — got {credential:?}"
    );
    for c in ["value", "name"] {
        assert!(credential.iter().any(|have| have == c), "`credential.{c}` is NOT NULL too");
    }

    // A column with a DEFAULT is NOT required: it reaches an old store through `ALTER TABLE`
    // and the rebuild legitimately fills it from the column's own default.
    for c in ["secret", "superseded_at", "notes", "venue", "venue_id", "account_id"] {
        assert!(
            !credential.iter().any(|have| have == c),
            "`credential.{c}` is nullable or defaulted, so requiring it would refuse a rebuild \
                 of every store that predates it — which is the whole population this runs on"
        );
    }
    assert!(
        !required_columns_of("account").iter().any(|c| c == "armed" || c == "active"),
        "`armed`/`active` are `NOT NULL DEFAULT`, the exact shape a store written before them \
             lacks"
    );

    // Every table in the batch must answer something, or a rebuild of it could produce an
    // EMPTY column list and an `INSERT INTO t () SELECT  FROM t` syntax error.
    for chunk in DDL.split("CREATE TABLE IF NOT EXISTS ").skip(1) {
        let table: String =
            chunk.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        assert!(
            !required_columns_of(&table).is_empty(),
            "`{table}` declares no NOT-NULL-without-DEFAULT column, so the intersection a \
                 rebuild copies could be empty and the parser cannot be trusted for it"
        );
    }
}

/// **A DECLINE has a reason, and the reason names the columns** — the half of the silent-skip
/// finding this crate can close on its own.
///
/// [`Rebuild::Skipped`] used to be a unit variant, so the three call sites could only turn it
/// into a bare `continue` and nothing anywhere knew WHY a store had not been repaired. This
/// pins that the note exists, names the table, and names the missing column — without it the
/// renderer is reached by no test at all and rots into an empty string nobody notices.
#[test]
fn a_declined_rebuild_carries_the_reason_it_declined() {
    assert_eq!(Rebuild::Done.decline_note("account"), None, "a rebuild that RAN has no note");

    let note = Rebuild::Skipped { missing: vec!["field".to_string()] }
        .decline_note("credential")
        .expect("a decline must render a reason");
    assert!(note.contains("`credential`"), "the note must name the table: {note}");
    assert!(note.contains("field"), "…and the column that is missing: {note}");
    assert!(
        note.contains("refused"),
        "…and what the operator will actually MEET, which is the next write being refused by \
             the constraint this repair was supposed to replace: {note}"
    );
}

/// **[`autoincrement_tables`] is DERIVED from the batch**, and this pins the two tables that
/// are deliberately absent so a silent arming of either is visible.
#[test]
fn the_armed_table_derivation_matches_the_shipped_batch() {
    let armed = autoincrement_tables();
    for table in ["node_key", "venue", "account", "credential", "venue_setting", "setting"] {
        assert!(armed.iter().any(|t| t == table), "`{table}` is armed in `DDL` — got {armed:?}");
    }
    assert!(armed.iter().any(|t| t == "profile_risk"), "…and `profile_risk`: {armed:?}");
    for table in ["settings_adoption", "venue_arming"] {
        assert!(
            !armed.iter().any(|t| t == table),
            "`{table}` must NOT be armed: `settings_adoption` is ruling 6's stated exception (a \
                 seal, `CHECK (id = 1)`) and `venue_arming` is a table §3 spells DELETED. Arming \
                 either also needs a `SEQUENCE_PIN` row in \
                 `crates/vike-secrets/tests/sqlite_sequence_gate.rs` — got {armed:?}"
        );
    }
}

/// **[`DROPPED_COLUMNS`] is the anti-vacuity half of §9 stage 4c**, and there are two ways it
/// could say nothing while [`migrate_dropped_columns`] ran happily on every write.
///
/// A row naming a column [`DDL`] STILL declares makes that migration rebuild the table on
/// every write forever and then refuse — the positive check would fire, an operator's
/// credential write would abort, and nothing before this test would have noticed. A row naming
/// a table [`DDL`] does not declare makes [`create_statement_under`] error out of a repair path
/// for a table nobody meant. So both halves are asserted, per row, against the shipped batch.
///
/// ⚠ It deliberately does NOT assert the count: the LENGTH of the array is the count, exactly
/// as the pinned tables in `crates/vike-ops/tests/` declare theirs, and a second statement of
/// it here would be the hand copy this tree keeps paying for.
#[test]
fn the_dropped_columns_are_absent_from_the_batch() {
    for (table, column, why) in DROPPED_COLUMNS {
        let decls = ddl_column_decls(table);
        assert!(
            !decls.is_empty(),
            "`{table}` must still be a table the shipped `DDL` declares — a row naming a table \
                 that has gone takes `create_statement_under` into an error on the repair path"
        );
        assert!(
            !decls.iter().any(|(name, _)| name == column),
            "`{table}.{column}` is still IN the shipped `DDL`, so `migrate_dropped_columns` \
                 would rebuild that table on every write and then refuse the operator's write when \
                 the column survived. Either take the column back out of `DDL` or delete this \
                 row — its stated measurement was: {why}"
        );
        assert!(
            !why.trim().is_empty(),
            "`{table}.{column}` must carry the measurement that licensed the drop — dead, or (the \
                 venue-links plan's text columns) redundant: a row here is a licence to destroy \
                 data on a live box"
        );
    }
}

/// **The one duplicated word, gated by MEMBERSHIP rather than by position.**
///
/// [`SIM_KEY_TOKEN`] is spelled out here and also lives in
/// `vike_model::credential_keys::CREDENTIAL_TIERS`. Taking it as `CREDENTIAL_TIERS[0]` would
/// read as *the first tier*, which is not what it is, and would silently re-point at `DEMO` if
/// that table were ever reordered. This asserts what is actually true — that the token is one
/// of the credential-key tiers — so a reorder is invisible and a REMOVAL is red.
#[test]
fn the_paper_tier_is_spelled_sim_in_a_credential_key() {
    assert!(
        vike_model::credential_keys::CREDENTIAL_TIERS.contains(&SIM_KEY_TOKEN),
        "`SIM_KEY_TOKEN` must be a real credential-key tier: {:?}",
        vike_model::credential_keys::CREDENTIAL_TIERS
    );
    assert!(
        !ACCOUNT_TIERS.contains(&SIM_KEY_TOKEN),
        "…and it is NOT an account tier — the whole point of §4.4 is that the two vocabularies \
             are different words, not one word in two cases"
    );
}

/// **Every credential-key tier token maps onto an [`ACCOUNT_TIERS`] member**, which is the
/// property `AccountResolver`'s [`SchemaRefusal::UnknownTier`] would otherwise fire on for a
/// perfectly ordinary key. Written as a loop over the real table so a FOURTH tier token cannot
/// be added upstream without this going red.
#[test]
fn every_credential_tier_token_names_an_account_tier() {
    for token in vike_model::credential_keys::CREDENTIAL_TIERS {
        let tier = account_tier_of_key_token(token);
        assert!(
            ACCOUNT_TIERS.contains(&tier.as_str()),
            "`{token}` maps to {tier:?}, which the `account` CHECK refuses"
        );
    }
}

/// **The two maps are inverses over the whole vocabulary**, which is what keeps a
/// `venue_setting` row's legacy credential NAME renderable from the tier the row stores. The
/// `paper` <-> `SIM` pair is the only one where they are not a case change, and it is the one
/// that silently broke a round trip before this landed.
#[test]
fn the_tier_and_the_key_token_round_trip() {
    for tier in ACCOUNT_TIERS {
        let token = key_token_of_account_tier(tier);
        assert_eq!(
            account_tier_of_key_token(&token),
            tier,
            "`{tier}` -> `{token}` -> … must come back to itself"
        );
        assert!(
            vike_model::credential_keys::CREDENTIAL_TIERS.contains(&token.as_str()),
            "`{tier}` renders the key token `{token}`, which no credential key is spelled with"
        );
    }
    assert_eq!(key_token_of_account_tier(PAPER_TIER), SIM_KEY_TOKEN, "the ONE non-case pair");
}

/// **The legacy INPUT spelling still classifies**, which is the migration for a dotted
/// `venue_setting` key an operator typed before the rename. It answers the CANONICAL word, so
/// `venue.ibkr.sim.backend` and `venue.ibkr.paper.backend` address the same row rather than
/// two.
#[test]
fn the_pre_rename_spelling_still_names_the_paper_tier() {
    for word in ["sim", "SIM", "Sim", "paper", "PAPER"] {
        assert_eq!(account_tier_named(word), Some(PAPER_TIER), "{word:?} must name the paper tier");
    }
    assert_eq!(account_tier_named("demo"), Some("demo"));
    assert_eq!(account_tier_named("live"), Some("live"));
    // …and a word that is not a tier answers `None` rather than being lowercased into one,
    // which is what lets the venue-settings grammar tell a tier segment from a FIELD.
    assert_eq!(account_tier_named("backend"), None);
    assert_eq!(account_tier_named("testnet"), None, "aster's own token is NOT a tier here");
}

// -----------------------------------------------------------------------------------------
// §5.2 step 7 — the `'any'` word and its two boundaries
// -----------------------------------------------------------------------------------------

/// **The two boundaries are inverses over every Rust tier**, and BOTH stored spellings of "no
/// tier" read as `None` — the NULL a store holds until its next write carries it, and the
/// `'any'` it holds afterwards. A read boundary that passed `'any'` through is the trap
/// step 7 was split off stage 4a for; one that mapped NULL to anything but `None` would change
/// what every UNMIGRATED box resolves.
#[test]
fn the_stored_tier_and_the_rust_tier_round_trip() {
    for tier in [None, Some("paper"), Some("demo"), Some("live")] {
        let stored = stored_venue_setting_tier(tier).expect("every Rust tier has a stored word");
        assert_eq!(
            venue_setting_tier_of_stored(Some(stored.to_string())).as_deref(),
            tier,
            "{tier:?} -> {stored:?} -> … must come back to itself"
        );
    }
    assert_eq!(
        stored_venue_setting_tier(None).expect("no tier"),
        ANY_TIER,
        "no tier is STORED as the word"
    );
    assert_eq!(venue_setting_tier_of_stored(None), None, "an unmigrated NULL still reads None");
    // …and the word is reached from `None` ALONE. A `Some("any")` passed through would file a
    // "tier" onto the machine-scoped row — the write boundary's own doc carries how that was
    // found — so the two directions are inverses only while this is refused.
    assert!(
        stored_venue_setting_tier(Some(ANY_TIER)).is_err(),
        "`Some(\"any\")` must be refused, not stored as the machine-scoped row's word"
    );
}

/// **`'any'` is a STORED word, never a tier** — so it can collide with neither vocabulary
/// the dotted-key grammar classifies by, and an account can never be filed under it.
#[test]
fn the_any_word_is_not_a_tier() {
    assert!(!ACCOUNT_TIERS.contains(&ANY_TIER), "`'any'` must not be an account tier");
    assert_eq!(
        account_tier_named(ANY_TIER),
        None,
        "`venue.<v>.any.<f>` must not be read as a tier-scoped key — `any` stays in the FIELD"
    );
}

/// **[`RETIRED_TIER_INDEXES`] must name indexes the shipped batch does NOT declare** — the
/// anti-vacuity half of step 7's second trigger, the twin of [`DROPPED_COLUMNS`]' own. A name
/// the batch still declared would be re-created by the closing `DDL` pass of every rebuild, so
/// the trigger would fire on every write forever and the positive check would then refuse it —
/// an operator's credential write aborted by a repair that can never finish.
#[test]
fn the_retired_tier_indexes_are_absent_from_the_batch() {
    for index in RETIRED_TIER_INDEXES {
        assert!(!DDL.contains(index), "`{index}` is retired but the shipped DDL declares it");
    }
    // ⚠ Keyed on the NUMBER since the venue-links plan's second release dropped the text `venue`
    // and the text-keyed `UNIQUE (venue, tier, field)` with it; the number's twin had stood beside
    // it since the first release.
    assert!(
        DDL.contains("UNIQUE (venue_id, tier, field)"),
        "…and the total UNIQUE that replaced them must be there"
    );
}
