//! The tier vocabulary: `ACCOUNT_TIERS`, the `SIM` key-token maps, and the stored word for a machine-scoped `venue_setting` row.

#[cfg(doc)]
use super::*;

/// **The tier vocabulary of [`AccountKey::tier`]** — the arming ceiling's spelling, lowercase.
///
/// §4's DDL comment names it *"paper | demo | live (the arming ceiling's vocabulary)"*, i.e.
/// `vike_config::VenueMode`'s, and the column is CHECK-constrained against this list because
/// `STRICT` constrains TYPES and not values — so without it the first reader to join
/// `account.tier` against a `VenueMode` would be comparing `"DEMO"` with `"demo"` and nothing in
/// the store would have objected.
///
/// ⚠ **This list spelled `paper` as `sim` until the 2026-09-23 rename, and the word was the only
/// difference — ruling 7 of
/// `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md`: *"One word for one idea
/// — no real broker connection."* §4.4 names exactly three sites: this constant, the DDL's
/// `CHECK`, and the KEY-NAME PARSER.** The comment quoted above is therefore now literal, and the
/// list no longer differs from `vike_config::VenueMode`'s vocabulary at all.
///
/// # ⚠ The KEY TOKEN did not move, and it must not be "finished"
///
/// `vike_model::credential_keys::CREDENTIAL_TIERS` is still `["SIM", "DEMO", "LIVE"]`, because
/// those are the words an OPERATOR TYPED into a credential key name: renaming them would make
/// every `{VENUE}_SIM_*` key on an existing box unreadable, which is a data migration wearing a
/// rename. §4.4 rules the other way and names its own precedent — *"legacy `MAINNET` key names
/// already load as `Live`"* — so the `SIM` token MAPS onto this `paper` tier exactly as `MAINNET`
/// maps onto `LIVE`. [`account_tier_of_key_token`] is that map, in one place, and
/// [`key_token_of_account_tier`] is its inverse for the renderer that has to go back.
///
/// The one deliberate difference from `VenueMode`'s vocabulary that REMAINS: a venue with no
/// credential at all still has no `account` row, because a row is minted by a credential. `paper`
/// here does not mean *this venue is unarmed*; it means *this account's credentials reach no real
/// broker*. `armed` is the separate column that says whether the operator allows it (§3.2 composes
/// the two as `armed ? tier : paper`).
pub const ACCOUNT_TIERS: [&str; 3] = ["paper", "demo", "live"];

/// **The account tier a credential key's TIER TOKEN names** — `SIM` → `paper`, everything else
/// lowercased.
///
/// ⚠ **This is the whole of §4.4's "key-name parser" half, and it is a MAP rather than a rename**
/// for the reason [`ACCOUNT_TIERS`] states: the token is what an operator typed. `token` arrives
/// already normalized by `vike_model::accounts::account_keys::AccountRef::tier`, which folds the legacy
/// `MAINNET` spelling onto `LIVE` — so this function sees at most the three
/// `vike_model::credential_keys::CREDENTIAL_TIERS` spellings and answers one of [`ACCOUNT_TIERS`].
///
/// A token it does not recognise is lowercased and handed on UNCHANGED rather than guessed at:
/// [`AccountResolver`] refuses a tier outside [`ACCOUNT_TIERS`] by name
/// ([`SchemaRefusal::UnknownTier`]), which is a better failure than silently filing a key against
/// the wrong tier. Aster's `TESTNET` and dukascopy's `DEMO1` are the two live families that arrive
/// here unrecognised — both are classified by `crate::venue_setting::HAND_MAPPED_ACCOUNTS` before
/// the grammar is consulted, so neither actually reaches this arm.
#[must_use]
pub fn account_tier_of_key_token(token: &str) -> String {
    account_tier_named(token).map_or_else(|| token.to_ascii_lowercase(), ToString::to_string)
}

/// **THE TIER VOCABULARY, in one function** — the [`ACCOUNT_TIERS`] member `word` names, or `None`
/// when it names none. Case-insensitive, so it answers for a credential key's uppercase `SIM` and
/// for a dotted settings key's lowercase `sim` alike.
///
/// ⚠ **It accepts TWO spellings of `paper` and that is a legacy INPUT spelling, not an alias.** The
/// distinction is the one `vike_model::credential_keys::LEGACY_CREDENTIAL_TIERS` already draws for
/// `MAINNET`: nothing in this workspace WRITES `sim` any more — [`migrate_sim_tier_to_paper`]
/// rewrites the stored rows and this crate renders [`PAPER_TIER`] everywhere — but two spellings
/// are already on disk in places no migration reaches:
///
/// * a credential key an operator typed, `{VENUE}_SIM_API_KEY`, whose token this workspace
///   deliberately does not rename ([`ACCOUNT_TIERS`] says why);
/// * a dotted `venue_setting` key an operator typed, `config.venue.ibkr.sim.backend`, which
///   `crate::venue_setting::parse_venue_setting_key` classifies by THIS vocabulary — so dropping
///   the old spelling would reclassify it from a tier-scoped row to a MACHINE-scoped one whose
///   field is `SIM.BACKEND`. Nothing errors; the operator's key simply addresses a different row.
#[must_use]
pub fn account_tier_named(word: &str) -> Option<&'static str> {
    if word.eq_ignore_ascii_case(SIM_KEY_TOKEN) || word.eq_ignore_ascii_case(SIM_TIER_WORD) {
        return Some(PAPER_TIER);
    }
    ACCOUNT_TIERS.iter().copied().find(|t| t.eq_ignore_ascii_case(word))
}

/// **The credential-key TIER TOKEN an account tier is spelled with** — the inverse of
/// [`account_tier_of_key_token`], and the reason it has to exist.
///
/// [`crate::venue_setting::venue_setting_names`] composes a LEGACY CREDENTIAL NAME out of a
/// `(venue, tier, field)` row — `{HEAD}_{TOKEN}_{FIELD}` — so it needs the token, not the tier. A
/// renderer that uppercased the tier word instead would emit `{HEAD}_PAPER_{FIELD}`, a name no
/// store has ever held, and the row it was rendering would become UNREACHABLE rather than wrong:
/// nothing errors, the old key simply stops answering. That is the silent half of this rename and
/// the reason the map is a pair rather than a single direction.
#[must_use]
pub fn key_token_of_account_tier(tier: &str) -> String {
    if tier.eq_ignore_ascii_case(PAPER_TIER) {
        SIM_KEY_TOKEN.to_string()
    } else {
        tier.to_ascii_uppercase()
    }
}

/// The `paper` member of [`ACCOUNT_TIERS`], named so the two maps above and the migration below
/// cannot disagree about its spelling.
pub const PAPER_TIER: &str = ACCOUNT_TIERS[0];

/// The credential-key tier token [`PAPER_TIER`] is spelled with.
///
/// ⚠ Spelled out rather than taken as `vike_model::credential_keys::CREDENTIAL_TIERS[0]` — the
/// dependency exists (`crate::venue_setting` already names that table) and the index would
/// resolve, but it would read as *the first tier*, which is not what this is. The two are held
/// equal by `schema_tests::the_paper_tier_is_spelled_sim_in_a_credential_key` instead, which
/// asserts MEMBERSHIP rather than position, so reordering that table cannot silently re-point this
/// constant at `DEMO`.
pub const SIM_KEY_TOKEN: &str = "SIM";

/// **The `venue_setting.tier` word for a MACHINE-SCOPED row** — "applies to any tier", which SQL
/// NULL spelled until §5.2 step 7 of
/// `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md` (owner ruling 2: a NULL
/// may not decide what KIND of row this is).
///
/// ⚠ **It is a STORED word and never a tier.** It is not in [`ACCOUNT_TIERS`], an account can
/// never carry it (`account`'s `CHECK` refuses it), and [`account_tier_named`] does not answer for
/// it — so a dotted key an operator types, `venue.polymarket.any.proxy_host`, does NOT address the
/// `(polymarket, None, PROXY_HOST)` row: `crate::venue_setting::parse_venue_setting_key` keeps the
/// `any` as part of the FIELD. Nothing on the Rust side spells "no tier" as anything but `None`.
/// The two functions below are the ONLY places the word meets that `None`, one per direction, and
/// every Rust value bound to or read from `venue_setting.tier` goes through one of them. (The
/// migration's own SQL — the carry and the collision check — spells the word in SQL, from this
/// constant.)
pub(crate) const ANY_TIER: &str = "any";

/// **The WRITE boundary** — the value a `venue_setting.tier` column stores for a row whose tier is
/// `tier` (`None` for a machine-scoped row).
///
/// ⚠ Every writer binds THIS rather than the `Option` itself. Binding the `Option` wrote SQL NULL,
/// which the shipped `tier TEXT NOT NULL` now refuses outright — the loud half of forgetting it.
/// The quiet half is a LOOKUP (`WHERE … tier IS ?`) that stays well-formed and simply finds nothing
/// for a row that plainly exists; `crate::settings::set_venue_setting_in`'s previous-value read is
/// that shape, and it binds this function too.
///
/// # ⚠ `Some(ANY_TIER)` is REFUSED — the map is a bijection or it is two spellings
///
/// This was `tier.unwrap_or(ANY_TIER)` when step 7 landed, which passed a `Some("any")` straight
/// through: the `CHECK` admits the word, so the write filed it as the MACHINE-SCOPED row,
/// overwrote that row and reported its old value, with nothing erroring. Before step 7 the stored
/// `CHECK` (`tier IS NULL OR tier IN ('paper', 'demo', 'live')`) refused the word, and
/// `set_venue_setting_in`'s `# Errors` section still promised that refusal — so the widening was
/// the step's own, found in review. No production caller can pass it
/// (`crate::venue_setting::parse_venue_setting_key` classifies by [`account_tier_named`], which
/// does not know the word); the refusal is for the next caller, and it is here rather than in each
/// writer so that it has one spelling. `crates/vike-secrets/tests/migration/venue_setting_any_tier.rs`'s
/// `the_stored_word_is_refused_as_a_tier_by_the_venue_setting_writer` holds it.
///
/// # Errors
/// A `SQLITE_CONSTRAINT` failure for `Some(ANY_TIER)`, shaped like the engine's own refusals so a
/// writer maps it the way it maps a `CHECK` violation.
pub(crate) fn stored_venue_setting_tier(tier: Option<&str>) -> rusqlite::Result<&str> {
    match tier {
        None => Ok(ANY_TIER),
        Some(ANY_TIER) => Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some(format!(
                "`{ANY_TIER}` is not a tier: it is the word a machine-scoped `{ANY_TIER_TABLE}` \
                 row is STORED under, and filing it as a tier would overwrite that row — pass \
                 `None` for the machine-scoped row, or one of paper/demo/live for a tier-scoped \
                 one"
            )),
        )),
        Some(tier) => Ok(tier),
    }
}

/// **The READ boundary** — the tier a stored `venue_setting.tier` value means, `None` for a
/// machine-scoped row.
///
/// ⚠ **BOTH spellings of "no tier" read as `None`, and that is not leniency.** A store the step-7
/// migration has not reached yet still holds SQL NULL — the migration runs on a store's next WRITE
/// (it rides `crate::db::ensure_venue_id_columns`), and every reader before that write meets the
/// old shape — so NULL must keep reading as `None`, which it does for free. The value that must
/// never get PAST this function is `'any'`: that is the trap step 7 was split off stage 4a for.
/// `crate::venue_setting::venue_setting_names` renders `{HEAD}_ANY_{FIELD}` from an unmapped
/// `'any'`, a name no store holds, and every legacy machine-scoped row goes UNREACHABLE — not wrong,
/// invisible. `crates/vike-secrets/tests/migration/venue_setting_any_tier.rs` proves it THROUGH the public
/// readers rather than through a `SELECT`, which is the only way it can be proved.
pub(crate) fn venue_setting_tier_of_stored(stored: Option<String>) -> Option<String> {
    stored.filter(|tier| tier != ANY_TIER)
}

/// The one table step 7 reshapes.
pub(super) const ANY_TIER_TABLE: &str = "venue_setting";

/// **The ACCOUNT-TIER word [`PAPER_TIER`] replaced** — the value `account.tier` and
/// `venue_setting.tier` used to hold, lowercase.
///
/// Nothing in this workspace WRITES it as a tier any more. Three sites name this constant and all
/// three are legitimate: [`migrate_sim_tier_to_paper`]'s trigger (*is the old word still in the
/// stored `CHECK`*) and its `CASE` rewrite, and [`account_tier_named`], which accepts it as a
/// legacy INPUT spelling for the reason that function's own doc measures — a dotted
/// `venue_setting` key an operator typed before the rename.
///
/// # ⚠ A `sim` found elsewhere is usually NOT a missed site — this doc said it always was
///
/// The previous wording (*"a reader who finds it anywhere else has found a site this rename
/// missed"*) is over-broad, and acting on it has already cost one wrong instruction: `sim` names
/// **three different things** in this tree and §4.4 renamed exactly one of them. A
/// production-versus-test split does not separate them; the question that does is **what the value
/// IS**. The two vocabularies the rename deliberately left alone:
///
/// * **the credential-key TIER TOKEN** — `crates/vike-model/src/credential_keys.rs`'s
///   `CREDENTIAL_TIERS` keeps `SIM`, because that is what an operator TYPED into a key NAME, and
///   renaming it would make every `{VENUE}_SIM_*` key on an existing box unreadable
///   ([`ACCOUNT_TIERS`] carries the ruling, [`SIM_KEY_TOKEN`] is this crate's name for the token,
///   and [`account_tier_of_key_token`] is the map onto the tier). ⚠ It is not always UPPERCASE on
///   screen: `crates/vike-app-core/src/ui/tool_views/venues.rs`'s `credentials_cell` renders it
///   lowercase beside `demo` and `live`, and its own doc records the refusal to "fix" it — that
///   cell answers *which credential key sets exist*, so relabelling it `paper` would put a word on
///   screen that appears in no key the operator can write.
/// * **the VENUE STRING** — `sim` is the simulated venue the backtest and paper exec planes tag an
///   order with (`crates/vike-backtest/profiles/wf_momentum.toml` documents it as the default for
///   a run's `venue` key; `crates/vike-exec/src/account.rs`'s `Account` takes it as one). It is a
///   venue id, not a tier, it is not a `vike_model::VENUES` member, and nothing in that plane ever
///   compares it against [`ACCOUNT_TIERS`].
///
/// **What IS a missed site**, and the only thing worth grepping for: a lowercase `sim` used as an
/// ACCOUNT TIER — stored into, compared against, or rendered for `account.tier` or
/// `venue_setting.tier`. Anything else is one of the two vocabularies above, and the check is the
/// value's TYPE at that call site rather than the word.
pub(super) const SIM_TIER_WORD: &str = "sim";
