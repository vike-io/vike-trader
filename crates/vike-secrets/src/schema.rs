//! **Schema 2 — the account is a ROW.** The DDL of
//! `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §4 and §6, the derivation that
//! fills it, and the in-place reshape that carries a schema-1 store into it.
//!
//! Schema 1 was two flat `(name, value)` tables. What it could not say is the thing §1 of that spec
//! is about: WHICH ACCOUNT a credential belongs to. The account lived in the key NAME
//! (`DUKASCOPY_DEMO1_LOGIN` bakes an account INDEX into the tier token), so an account could go
//! stale with nothing in the store able to contradict it. Schema 2 gives the account a row, a
//! permanent opaque `id`, and a place for the venue's own answer (`venue_account_id`).
//!
//! # ⚠ What this module does NOT do, and why that is the spec's own rule rather than a shortcut
//!
//! §12 states it as a hard sequencing requirement rather than a follow-up:
//!
//! > Ruling 10 moves ten values out from under the readers that find them today; §6.2's ordering
//! > (A) pays for that with the renderer and ordering (B) with four bridge PRs, but NEITHER is
//! > written and until one is, **THE ROWS MAY NOT MOVE.**
//!
//! So §11's steps **3 and 4 are not performed here**. The ten book keys are NOT folded into
//! `account.venue_account_id` and the ten config-shaped keys are NOT moved to `venue_setting`;
//! every one of them stays a `credential` row carrying its legacy `name`, and
//! [`Classification::pending_move`] is how each one says so BY NAME in the migration's report. The
//! consequence is the property this whole change rests on: **`credential` still holds every live
//! name**, so [`crate::db::read_table`] answers with the same map before and after, and no renderer
//! is needed to make that true. `venue_setting` is created EMPTY for the same reason — a table is a
//! SHAPE, and a ROW nothing reads is the defect `vike_config::CONSUMPTION` exists to refuse.
//!
//! It also writes no `venue_account_id`, on either dukascopy row. See [`DDL`]'s `label` note.
//!
//! # The classification is INJECTED, exactly like `is_node_key`
//!
//! `field`, `account_id` and `venue` are derived from the key NAME, and the production
//! implementation of that derivation is `vike_bridge_core::credentials::classify_credential_name`
//! — a function of a crate this one **cannot name, in two independent ways**. `vike-bridge-core`
//! declares `layer = 30` where this crate declares `15`, and dependency direction is DOWN ONLY and
//! machine-checked (`crates/vike-ops/tests/layer_gate.rs`); and that crate declares `vike-secrets`
//! as a normal dependency, so the reverse edge is a cycle as well as a layer violation. The
//! derivation therefore arrives as a closure returning [`Classification`] — the same seam, for the
//! same reason, that [`crate::db::migrate`] already takes `is_node_key` through.
//!
//! ⚠ **The reason above REPLACES the one this paragraph used to give, whose premise expired.** It
//! read *"**This crate declares no `vike-*` dependency** (`crates/vike-secrets/Cargo.toml`'s
//! layer-15 `leaf` tier, and two consumers rest on it)"*, and that is false since
//! `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md` (accepted
//! 2026-09-20): the manifest declares `vike-model` as a normal dependency and
//! `crates/vike-secrets/src/db.rs`'s `ensure_venue_rows` iterates `vike_model::venues::VENUES`
//! outright. ⚠ Read 0072's *"Layers 15 → 10, strictly down"* as the EDGE's direction rather than as
//! this crate moving: `crates/vike-secrets/Cargo.toml` still declares `layer = 15`, and `10` is
//! `vike-model`'s.
//!
//! ⚠ **That is not licence to collapse the seam.** 0072 admitted ONE edge, on the ground that it
//! costs nothing in any graph, and ruled nothing about injected seams. The layer bound above is why
//! this one stands whatever the manifest gains next — and it is a DIFFERENT argument from the one
//! that expired, not a restatement of it.

use std::collections::BTreeMap;

use rusqlite::Transaction;

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
/// already normalized by `vike_model::account_keys::AccountRef::tier`, which folds the legacy
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
/// equal by [`schema_tests::the_paper_tier_is_spelled_sim_in_a_credential_key`] instead, which
/// asserts MEMBERSHIP rather than position, so reordering that table cannot silently re-point this
/// constant at `DEMO`.
pub const SIM_KEY_TOKEN: &str = "SIM";

/// **Which account a credential name belongs to**, as the classifier reports it.
///
/// `venue`/`tier`/`label` are `vike_model::account_keys::AccountRef`'s three fields — §4.1's
/// `UNIQUE (venue, tier, label)` is that tuple and not an invention — carried as owned `String`s
/// because this crate cannot name that type.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AccountKey {
    /// A `vike_model::VENUES` id.
    pub venue: String,
    /// One of [`ACCOUNT_TIERS`].
    pub tier: String,
    /// The operator's name for the ROLE. **`None` is the ordinary answer and is what the migration
    /// writes for every account it creates**, because `AccountLabel::Default` renders as ABSENT
    /// everywhere else in this workspace — its serde impl skips it, and
    /// `AccountLabel::parse("DEFAULT")` REFUSES the reserved spelling outright, so the literal
    /// string would not round-trip.
    pub label: Option<String>,
    /// ⚠ **Derivation-time only — it is stored in NO column.**
    ///
    /// Two accounts of one venue at one tier are indistinguishable by `(venue, tier, label)` when
    /// both labels are absent, and dukascopy is exactly that case: `DEMO1` and `DEMO2` are two
    /// accounts (ruling 1) and the owner ruled that neither gets a label. This field lets the
    /// classifier's HAND-MAP say *these two names belong to different accounts* without putting the
    /// index token into a column — which is the defect §1 is about, and §11.1's own rejected
    /// alternative. `None` for every other venue.
    pub discriminator: Option<String>,
}

/// Where a credential row sits — §5.1's three cases, as the pair `(account_id, venue)` expresses
/// them. **The pair IS the classification; there is no separate kind column to keep in step.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// `account_id` set, `venue` NULL — the venue is on the account row. `OKX_DEMO_API_SECRET`.
    Account(AccountKey),
    /// `account_id` NULL, `venue` set — an APPLICATION credential shared by every account of the
    /// venue. `CTRADER_CLIENT_ID`. ⚠ `venue` here means *belongs to this venue's plane*, not
    /// *issued by this venue*.
    Venue(String),
    /// Both NULL — the deployment's own credentials. `CLOUDFLARE_API_TOKEN`.
    Infrastructure,
}

/// A row this change deliberately does not move, and the spec section that will.
///
/// Reported BY NAME so the next change takes its work-list out of a run rather than out of prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PendingMove {
    /// §7 — this value IS the book, and folds into `account.venue_account_id` once the map renderer
    /// can re-synthesize its legacy name.
    BookIdentifier,
    /// §6 / ruling 10 — machine- or tier-scoped configuration that holds no secret, and moves to
    /// `venue_setting` behind the same renderer.
    VenueSetting,
}

impl std::fmt::Display for PendingMove {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PendingMove::BookIdentifier => "would fold into account.venue_account_id (spec 7)",
            PendingMove::VenueSetting => "would move to venue_setting (spec 6, ruling 10)",
        })
    }
}

/// **What one credential NAME is**, as the injected classifier answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    /// §5.1's three cases.
    pub placement: Placement,
    /// §4.4 — the name with its OWNER PREFIX removed; the WHOLE name for an infrastructure row,
    /// whose owner is the deployment.
    pub field: String,
    /// `false` for the rows §6 MEASURED as holding no secret. `true` everywhere else: a value
    /// wrongly marked non-secret is a worse error than one wrongly marked secret.
    pub secret: bool,
    /// `false` when the classifier could not place this name and fell back to
    /// [`Placement::Infrastructure`]. The row is still written with `name` and `value` VERBATIM —
    /// **a name the parse cannot classify is never dropped and never guessed at** — and it is
    /// REPORTED by name (§11.1).
    pub recognised: bool,
    /// Set when this row is one §7 or §6 will move later. See [`PendingMove`].
    pub pending_move: Option<PendingMove>,
}

impl Classification {
    /// The unrecognised fallback — §11 step 6, and the shape a classifier returns when it has
    /// nothing to say about a name.
    #[must_use]
    pub fn unrecognised(name: &str) -> Classification {
        Classification {
            placement: Placement::Infrastructure,
            field: name.to_string(),
            secret: true,
            recognised: false,
            pending_move: None,
        }
    }

    /// The OWNER PREFIX this classification implies — the name with [`Classification::field`]
    /// removed from its end.
    ///
    /// ⚠ **This is the account's re-derivable identity ACROSS RUNS**, and it is why the migration
    /// needs no stored discriminator. Every credential row keeps its legacy `name` (§4.1: *"`name`
    /// is the only record"*) and its `field`, so an account's owner prefix — `DUKASCOPY_DEMO1_` —
    /// is recoverable from any one of its own rows. A later run therefore finds the account an
    /// earlier run created, without either of them putting the index token in a column.
    ///
    /// ⚠ `None` for a LABELLED account, and that is not a failure: a labelled key's `field` is not
    /// a suffix of its name (the `__LABEL` sits after it), and a labelled account does not need
    /// this lookup — `(venue, tier, label)` is unique for it, so [`AccountResolver`]'s SECOND
    /// lookup answers. The prefix exists for the one case nothing else can answer: an UNLABELLED
    /// account that shares `(venue, tier, label)` with another, i.e. dukascopy's pair.
    ///
    /// ⚠ It is ALSO `None` when a classifier contradicts itself by naming a `field` that is no
    /// part of the name's end — and **that is refused only where it is not survivable**, which is
    /// the discriminated-unlabelled shape above. This doc said it "IS refused" flatly, and
    /// [`SchemaRefusal::FieldIsNotASuffix`]'s own doc said the same; both were wider than
    /// [`write_rows`], which raises that refusal under
    /// `Classification::needs_owner_prefix` alone. A blanket refusal is not
    /// available and never was: a LABELLED key's `field` is legitimately not a suffix of its name,
    /// so refusing every non-suffix would refuse the whole labelled grammar.
    #[must_use]
    pub fn owner_prefix<'a>(&self, name: &'a str) -> Option<&'a str> {
        name.strip_suffix(self.field.as_str())
    }

    /// Is this an account-scoped row whose account can ONLY be found by its owner prefix?
    ///
    /// True exactly when the account is unlabelled AND the classifier offered a discriminator —
    /// the dukascopy shape, where `(venue, tier, label)` has two answers and the discriminator
    /// reaches no column. For every other row a missing prefix is survivable.
    fn needs_owner_prefix(&self) -> bool {
        matches!(
            &self.placement,
            Placement::Account(key) if key.label.is_none() && key.discriminator.is_some()
        )
    }
}

/// **The schema-2 DDL — §4's two tables, §6's third, and `node_key` unchanged.**
///
/// Stated once, here, and applied both by a fresh create and by [`reshape_into`], so a store BORN
/// at 2 and a store CARRIED to 2 cannot end up different shapes.
///
/// # Three deviations from §4's printed DDL, each argued
///
/// **1. `label` is NULLABLE, where §4 prints `label TEXT NOT NULL`.** The signature at the head of
/// that spec rules that the provisional `DEMO1`/`DEMO2` labels *"are NOT written at all (labels are
/// informative and optional, `id` is the identity…)"*. That ruling and a `NOT NULL` label cannot
/// both hold: §11.1's surviving rule derives `RESERVED_DEFAULT_LABEL` for every account, dukascopy
/// has TWO accounts at `(dukascopy, demo)`, and `UNIQUE (venue, tier, label)` would then refuse the
/// sixteenth row — §4.1 says exactly that in its own words (*"Adding `tier` fixes hyperliquid and
/// does NOT fix dukascopy"*), and the signature removed §11.1's answer without supplying a
/// replacement. Nullable is also the FAITHFUL mirror rather than a workaround: everywhere else in
/// this workspace `AccountLabel::Default` is rendered as ABSENT, and
/// `vike_model::account_keys::AccountLabel::parse` REFUSES the literal `"DEFAULT"` as reserved, so
/// a stored `'DEFAULT'` string would not parse back.
///
/// ⚠ **The cost is stated rather than hidden.** With every migrated label NULL,
/// `UNIQUE (venue, tier, label)` constrains NOTHING (NULLs are distinct in a SQLite index), so what
/// keeps a re-run from inserting a duplicate account is [`AccountResolver`]'s name- and
/// prefix-keyed lookup plus `credential_one_live_name`, not this index. The index starts biting the
/// moment an account is LABELLED, which is the state it was written for.
///
/// **2. `CHECK (tier IN …)`** — §4's comment names the vocabulary and `STRICT` cannot enforce it.
/// See [`ACCOUNT_TIERS`] for why the list IS the comment's three words since the 2026-09-23 rename
/// and is still not `CREDENTIAL_TIERS`' three. The two `IN (0, 1)` checks are the same reasoning
/// applied to the two boolean columns, which `STRICT` types as `INTEGER` and would otherwise let
/// hold `7`.
///
/// **3. `venue_setting` is created EMPTY** — see this module's doc. The table is the SHAPE the
/// signature accepted; the step that FILLS it is §11 step 4, which §12 forbids until the map
/// renderer exists.
///
/// **4. `account`, `credential`, `venue_arming` and `venue_setting` each carry a nullable
/// `venue_id INTEGER REFERENCES venue(id)` beside their text `venue` column** — a DUAL-WRITE half
/// with no reader yet. ⚠ **Nothing reads it; the text column stays authoritative.** It exists so a
/// later change can reshape onto the typed link without a flag day: EVERY writer of a row carrying a
/// text `venue` fills it beside its text sibling — [`AccountResolver::resolve`]'s and
/// [`crate::db::edit_account`]'s account INSERTs, [`write_rows`]'s two `credential` INSERTs that can
/// ever carry a non-NULL `venue` (its other two sit inside the `Placement::Account` branch, where
/// `venue` is provably `None` — see the comment at that branch's own `if let`), and
/// `write_settings`'s/`set_venue_setting_in`'s/[`crate::db::move_pending_rows`]'s `venue_arming`/
/// `venue_setting` INSERTs — [`crate::db`]'s `ensure_venue_id_columns` backfills a store that
/// predates it (and is ALSO called directly by the four writers that never pass through
/// [`crate::db::fill_into`], so none of them can hit a missing column on an existing store), and
/// `venue_id_agrees_with_the_text_column_on_every_row` (`crates/vike-secrets/tests/venue_table.rs`)
/// pins that the two never disagree on any row, across all four tables.
/// `crates/vike-secrets/tests/store_link_gate.rs` does not count these four as untyped links (its
/// `observed()` skips any column carrying `REFERENCES`), so its pin stays at the four TEXT `venue`
/// columns it already named.
///
/// Everything else is §4 and §6 verbatim, including both partial indexes on `credential` (§4.1
/// argues why each must carry its `WHERE` — a plain `name … UNIQUE` would refuse the very
/// superseded rows §4.2 exists to keep), `venue_setting`'s two (NULLs are distinct, so ONE index
/// over `(venue, tier, field)` would say nothing at all about the machine-scoped rows), and the
/// `CHECK (account_id IS NULL OR venue IS NULL)` that refuses the fourth of §5.1's four
/// combinations.
///
/// `IF NOT EXISTS` throughout, for the reason schema 1's batch already gives: a database created by
/// a concurrent first run is JOINED rather than fought.
///
/// # ⚠ The last three tables are NOT the credential schema, and the boundary is [`crate::settings`]'s
///
/// `setting` and `venue_arming` arrive with
/// `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`'s Phase 1 and belong to
/// a different owner from everything above them. They are stated HERE for the reason this const's
/// opening line gives — one statement of shape, applied by every create path, so a store BORN
/// complete and a store CARRIED here cannot end up different — and the sentence that says which
/// table a new venue-scoped knob belongs in lives in [`crate::settings`]'s module doc, which is the
/// authority. ⚠ Neither is added to [`SCHEMA_VERSION`](crate::SCHEMA_VERSION): a bump would drop 2
/// out of [`READABLE_SCHEMA_VERSIONS`](crate::READABLE_SCHEMA_VERSIONS) and make both already-
/// migrated boxes' credential stores unreadable, which is the LIVE GATE. An already-migrated store
/// simply has no such table until a writer runs this batch again, and
/// [`crate::settings::read_settings`] answers [`crate::settings::SettingsSource::TablesAbsent`] for
/// it rather than pretending the rows are empty.
///
/// **`setting`'s `CHECK` is a closed VOCABULARY, never a bound.** `section IN (…)` is the same
/// reasoning as `tier IN (…)` above — four words a `STRICT` `TEXT` column cannot otherwise refuse.
/// A `CHECK` restating a NUMERIC bound (`max_leverage <= …`) is forbidden and deliberately absent:
/// `vike_config`'s typed model imports every bound from `vike_model`, and a second copy in SQL is
/// the split-brain that model exists to refuse.
///
/// **`venue_arming` carries no precedence column, no `overridable` flag and no per-row layer
/// field**, and that is a decision rather than an omission — 0057's `policy.toml` verdict in as
/// many words. A precedence column is how a ceiling acquires an override, and the seal that makes a
/// ceiling a ceiling is a property of the TYPE (`vike_config::layers`'s sealed `EnvOverride` /
/// `CliOverride`, which `Policy` does not implement), not of the carrier underneath the loader.
///
/// **`label` is NULLABLE and the two partial indexes are why**, exactly as `venue_setting`'s pair
/// above: NULLs are distinct in a SQLite index, so one `UNIQUE (venue, label)` would constrain
/// nothing about the venue-level rows. A NULL label is the `[venues]` row for that venue; a
/// non-NULL one is an `[accounts]` row for that venue's labelled account.
///
/// **`profile_risk` is 0057's Phase 2** — a RUN PROFILE's `[risk]` table, one row per key, and it
/// is a THIRD owner rather than a variant of either table above it. The boundary is spelled in
/// [`crate::settings`]'s module doc beside the other three; the two properties worth carrying at
/// the DDL itself:
///
/// * **`profile` is part of the key and is not optional**, because *which* profile is live is
///   0057's open Question 3 and this table must not answer it by accident. `VIKE_RUN_PROFILE` (or
///   `--profile`) names a PATH, a box may hold several, and a table keyed on `key` alone would be
///   "the risk budget" — i.e. an active-profile row written sideways. `UNIQUE (profile, key)` says
///   instead: these are the ceilings of the profile that goes by this name, and nothing here
///   selects one.
/// * **Nothing on the mount path reads it**, so no `CHECK` restates a numeric bound here either
///   (the rule the `setting` paragraph above states). The rows are a DISCLOSURE copy: the file the
///   boot resolves is still the only thing that builds a `vike_exec::ProfileRisk`, and
///   `crates/vike-ops/tests/profile_risk_readers_gate.rs` is what holds that true as the tree
///   changes. `vike_config::PROFILE_RISK_KEYS` is the key vocabulary, and it is a Rust roster
///   rather than a `CHECK` list for the reason `setting.key` has none: it is gated against
///   `vike_exec::ProfileRisk`'s own fields, and a SQL copy would be a second authority that drifts.
///
/// **`settings_adoption` is the PROBE, and it is a SEAL rather than a preference.**
///
/// One row, `CHECK (id = 1)`, written by `vike-cli config adopt` and by nothing else. Its presence
/// is the whole of the question *does this box resolve its settings from the rows or from the four
/// files* — decided ONCE per run, before any key is looked up, exactly as [`crate::Backend`]
/// decides which store answers for a credential NAME.
///
/// ⚠ **Why a row and not a count, a table probe or a `precedence` column**, each of which was
/// available and each of which is refused:
///
/// * A DATABASE-EXISTS or TABLES-EXIST probe is dead on arrival, and that is MEASURED rather than
///   argued: [`crate::db::open_for_write`] runs this whole batch on the `created` branch, so
///   `vike-cli secrets migrate` — a CREDENTIAL command — leaves `setting` and `venue_arming`
///   present and EMPTY on every fresh box. A probe shaped like either one would flip a box that
///   has mirrored nothing into resolving from zero rows, which is every ceiling absent and every
///   venue `paper` with `is_declared()` false.
/// * A ROW COUNT is a data fact with no declared intent behind it, so an accident moves the
///   crossing in both directions.
/// * A `precedence` COLUMN is refused for the reason the `venue_arming` paragraph above gives in
///   0057's own words: it puts the type's seal one `UPDATE` away from gone.
///
/// The columns are not disclosure. `venues_declared` is `VenuePolicy::is_declared` AT ADOPTION and
/// is the UNMOUNT detector — a `policy.toml` with no `[venues]` table mirrors to ZERO arming rows
/// deliberately, so *no arming rows* alone cannot be told from *the rows were erased*.
/// `setting_rows`/`arming_rows` are the ERASE detector: every sanctioned write updates them in its
/// own transaction, so a mismatch in either direction means rows moved by some other route.
/// `files_present` says what the box had when it crossed, which is what a later deletion is
/// deleting.
///
/// Like the three tables above it, NOT added to [`SCHEMA_VERSION`](crate::SCHEMA_VERSION) — same
/// live gate, and with a second property this one needs: an OLDER binary meeting an adopted store
/// does not see the table at all and resolves from the FILES, which is the safe direction for a
/// rollback.
///
/// Like the two before it, `profile_risk` is NOT added to [`SCHEMA_VERSION`](crate::SCHEMA_VERSION)
/// — the same live gate, measured the same way: a bump drops 2 out of
/// [`READABLE_SCHEMA_VERSIONS`](crate::READABLE_SCHEMA_VERSIONS) and makes both already-migrated
/// boxes' CREDENTIAL stores unreadable. Presence is asked of `sqlite_master`.
///
/// **`account.armed` is a DERIVED column — the operator's PERMISSION, materialized.**
///
/// `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md` §9 stage 3. `tier` is
/// what an account CAN reach (its credential set); `armed` is whether the operator allows it, and
/// §3.2 composes the two as `armed ? tier : paper`. The column is filled by
/// [`crate::settings::fold_arming_into_accounts`] and by NOTHING else — it is a pure function of
/// the `venue_arming` rows and the `account` rows, recomputed wholesale whenever either changes,
/// so there is no state here that can drift from the ceiling the mount actually reads.
///
/// ⚠ **`venue_arming` is NOT dropped by that fold, and §3's *DELETED* has not happened yet.** The
/// stage-2 shape is repeated deliberately: this is the dual-written half of a column whose READER
/// has not moved (the module doc of this file already names that shape for `venue_id` — *a
/// dual-write half with no reader yet*). What the reader still needs and `account` cannot yet
/// answer is a `[venues]` line for a venue that has NO account row — a venue with no credentials
/// has no row to hang a ceiling or a `max_exposure` figure on, and losing a figure means
/// UNBOUNDED. `crates/vike-secrets/src/settings.rs`'s `fold_arming_into_accounts` carries the
/// whole ruling.
///
/// ⚠ **Like every column added after the schema-2 freeze, this one reaches an EXISTING store
/// through an `ALTER TABLE`, not through this batch** — `CREATE TABLE IF NOT EXISTS` changes
/// nothing about a table that is already there. The `ALTER` cannot carry the table-level
/// `CHECK (armed IN (0, 1))` above, so a store born before this column has the column without the
/// constraint until stage 4's rebuild; the fold is the only writer and it writes `0`/`1`, which is
/// the same asymmetry `venue_id`'s nullable-only `REFERENCES` already carries.
///
/// # ⚠ `tier IN ('paper', …)` is the ONE post-freeze change an `ALTER TABLE` cannot carry at all
///
/// The paragraph above is about a column an existing store LACKS. The `sim` → `paper` rename
/// (§4.4, ruling 7) is the opposite shape and is strictly worse: the column is already there,
/// `CREATE TABLE IF NOT EXISTS` skips the table, and the CHECK an already-migrated store holds
/// still reads `('sim', 'demo', 'live')`. So on BOTH live boxes this batch would leave a table
/// that REFUSES the very word the classifier now produces — a `{VENUE}_SIM_*` credential write
/// failing with a bare `CHECK constraint failed` on a store that worked yesterday. SQLite has no
/// `ALTER TABLE … DROP CONSTRAINT`, so the repair is a table REBUILD, and
/// [`migrate_sim_tier_to_paper`] is it: one step that rewrites the ROWS and the CONSTRAINT
/// together, because they are the same migration.
///
/// ⚠ **MEASURED 2026-09-23: neither live box can reach that failure today** — the CI box and the dev
/// box each hold `0` `_SIM_` credential keys and `0` `tier = 'sim'` rows, so no `paper` row is
/// minted there and the old CHECK never fires. That is why the rename is safe to make now; it is
/// NOT why the migration can be skipped, because a store elsewhere may hold one and the failure it
/// would take is a write refusal, not a wrong answer.
///
/// # ⚠ The dead columns (§9 stage 4c) — ONE was dropped, and the others are NOT dead
///
/// §9's stage-4 row says *"the dead columns"* in the plural and §2.7 names exactly one:
/// `venue_arming.notes`. That is also the only one the sweep could prove — **zero readers AND zero
/// writers across the whole tree**, not merely across this crate — so it is the only one dropped.
/// [`DROPPED_COLUMNS`] carries it with its measurement, and [`migrate_dropped_columns`] is what
/// takes the drop to a store that already exists; this batch alone would reach a NEWBORN store
/// only.
///
/// Every other candidate stays, and **the note is the deliverable for a column that was not
/// dropped** — a reader who finds one of these unnamed in any `SELECT` must not conclude it is
/// spare. These are in the doc rather than in the SQL because the batch is a string literal whose
/// BODY is taken verbatim by [`create_statement_under`] and split on commas by
/// [`ddl_column_decls`]: an SQL comment inside it becomes part of every rebuilt table's stored
/// text and a bogus entry in that split.
///
/// * **`account.notes` and `credential.notes` are WRITE-ONLY, and write-only is the opposite of
///   dead.** [`write_rows`] fills both — its `account` INSERT and three of its four `credential`
///   INSERTs name the column, the fourth being the pre-schema-2 rollback copy, which has no
///   comment to attach — from [`FileComments::notes`], which is §4.3's harvest of the operator's own
///   comment blocks out of their `secrets.env`. Nothing SELECTs either one, and that is a RULE
///   rather than an omission: [`crate::db::Account`]'s own doc quotes it (*notes are for humans,
///   and code NEVER reads them — the moment code parses a note it is not a note*). Dropping them
///   destroys the only copy of that prose.
/// * **`account.last_verified_at` has a writer AND a reader**, and the sentence that says
///   otherwise is stale. `crates/vike-model/src/account_confirmation.rs`'s module doc still reads
///   *"`account.last_verified_at` has no writer anywhere in the tree"*; it was true when written
///   and stopped being true on 2026-09-15. The writer is [`crate::db::set_venue_account_id`] under
///   [`crate::db::BookSource::Handshake`] (an `UPDATE account SET last_verified_at`, and a second
///   one folded into its `COALESCE` form); the readers are [`crate::db::Account::last_verified_at`]
///   — selected by this module's sibling `account_columns` projection on every account read — and
///   `vike-cli secrets accounts`, which renders it as the *(never)* column an operator looks at.
/// * **`setting.notes`, `venue_setting.notes`, `profile_risk.notes` and `settings_adoption.notes`
///   have neither a reader nor a writer today**, and they still stay: §3 DECLARES every one of
///   them, so they are UNWRITTEN rather than dead, and dropping a column the signed design carries
///   would be a divergence to argue at the spec rather than a debt to pay here
///   (`crates/vike-ops/tests/settings_store_ddl_gate.rs`'s `GROWTH_GUIDANCE` states the order).
///   `venue_arming.notes` is not in that company for the one reason that makes it droppable: §3
///   spells that whole TABLE deleted, so there is no design-side column to contradict.
/// * Two more came out of the same sweep and are named so the next reader does not re-measure
///   them: **`account.parent_id` is READ-ONLY** (the projection above selects it into
///   [`crate::db::Account::parent_id`]; nothing writes it yet, and `crate::db::edit_account`'s own
///   comment says so), and **`credential.secret` is WRITE-ONLY** ([`write_rows`] again). Neither
///   is dead and §3 declares both.
pub const DDL: &str = "\
CREATE TABLE IF NOT EXISTS node_key (
    id    INTEGER PRIMARY KEY AUTOINCREMENT,
    name  TEXT NOT NULL UNIQUE,
    value TEXT NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS venue (
    id   INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE
) STRICT;

CREATE TABLE IF NOT EXISTS account (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    venue            TEXT    NOT NULL,
    venue_id         INTEGER REFERENCES venue(id),
    tier             TEXT    NOT NULL,
    armed            INTEGER NOT NULL DEFAULT 0,
    label            TEXT,
    venue_account_id TEXT,
    parent_id        INTEGER REFERENCES account(id),
    active           INTEGER NOT NULL DEFAULT 1,
    last_verified_at TEXT,
    notes            TEXT,
    UNIQUE (venue, tier, label),
    CHECK (tier IN ('paper', 'demo', 'live')),
    CHECK (armed IN (0, 1)),
    CHECK (active IN (0, 1))
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS account_one_account_per_book
    ON account (venue, venue_account_id)
    WHERE active = 1 AND venue_account_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS credential (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id    INTEGER REFERENCES account(id),
    venue         TEXT,
    venue_id      INTEGER REFERENCES venue(id),
    field         TEXT    NOT NULL,
    value         TEXT    NOT NULL,
    name          TEXT    NOT NULL,
    secret        INTEGER NOT NULL DEFAULT 1,
    superseded_at TEXT,
    notes         TEXT,
    CHECK (account_id IS NULL OR venue IS NULL),
    CHECK (secret IN (0, 1))
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS credential_one_live_value
    ON credential (account_id, field) WHERE superseded_at IS NULL;

CREATE UNIQUE INDEX IF NOT EXISTS credential_one_live_name
    ON credential (name) WHERE superseded_at IS NULL;

CREATE TABLE IF NOT EXISTS venue_setting (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    venue    TEXT NOT NULL,
    venue_id INTEGER REFERENCES venue(id),
    tier     TEXT,
    field    TEXT NOT NULL,
    value    TEXT NOT NULL,
    notes    TEXT,
    CHECK (tier IS NULL OR tier IN ('paper', 'demo', 'live'))
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS venue_setting_one_per_tier
    ON venue_setting (venue, tier, field) WHERE tier IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS venue_setting_one_per_machine
    ON venue_setting (venue, field) WHERE tier IS NULL;

CREATE TABLE IF NOT EXISTS setting (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    section TEXT NOT NULL,
    key     TEXT NOT NULL,
    value   TEXT NOT NULL,
    notes   TEXT,
    UNIQUE (section, key),
    CHECK (section IN ('policy', 'config', 'preferences', 'flags'))
) STRICT;

CREATE TABLE IF NOT EXISTS venue_arming (
    id       INTEGER PRIMARY KEY,
    venue    TEXT NOT NULL,
    venue_id INTEGER REFERENCES venue(id),
    label    TEXT,
    mode     TEXT NOT NULL,
    max_exposure REAL,
    CHECK (mode IN ('paper', 'demo', 'live')),
    CHECK (max_exposure IS NULL OR max_exposure > 0.0)
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS venue_arming_one_per_venue
    ON venue_arming (venue) WHERE label IS NULL;

CREATE UNIQUE INDEX IF NOT EXISTS venue_arming_one_per_account
    ON venue_arming (venue, label) WHERE label IS NOT NULL;

CREATE TABLE IF NOT EXISTS settings_adoption (
    id              INTEGER PRIMARY KEY,
    adopted_at      TEXT    NOT NULL,
    tool_version    TEXT    NOT NULL,
    files_present   TEXT    NOT NULL,
    venues_declared INTEGER NOT NULL,
    setting_rows    INTEGER NOT NULL,
    arming_rows     INTEGER NOT NULL,
    notes           TEXT,
    CHECK (id = 1),
    CHECK (venues_declared IN (0, 1))
) STRICT;

CREATE TABLE IF NOT EXISTS profile_risk (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    profile TEXT NOT NULL,
    key     TEXT NOT NULL,
    value   TEXT NOT NULL,
    notes   TEXT,
    UNIQUE (profile, key)
) STRICT;
";

/// The schema-1 `credential` table, re-created by [`reshape_into`] under a scratch name so the
/// rebuild is a copy rather than an in-place `ALTER`. SQLite cannot add or drop a PRIMARY KEY with
/// `ALTER TABLE`, and schema 1's `name TEXT PRIMARY KEY` has to become schema 2's surrogate `id`.
const RESHAPE_SCRATCH: &str = "credential_schema1";

// ---------------------------------------------------------------------------------------------
// §4.4 — the `sim` -> `paper` value AND constraint migration
// ---------------------------------------------------------------------------------------------

/// The two tables whose `tier` column carried the old word. Ordered for readability only — each is
/// rebuilt independently and nothing links them.
const RETIER_TABLES: [&str; 2] = ["account", "venue_setting"];

/// Suffix for the scratch name a table is renamed aside to while it is rebuilt.
const RETIER_SCRATCH_SUFFIX: &str = "_pre_paper_tier";

/// **Carry an EXISTING store onto [`ACCOUNT_TIERS`]' `paper`** — the rows AND the `CHECK` that
/// refuses them, in one idempotent step. A no-op on a store born after the rename.
///
/// # Why this is not an `UPDATE`
///
/// An `UPDATE account SET tier = 'paper' WHERE tier = 'sim'` runs against the table's OWN check
/// constraint, which on an already-migrated store still reads `CHECK (tier IN ('sim','demo',
/// 'live'))` — [`DDL`] is `CREATE TABLE IF NOT EXISTS` and changes nothing about a table that is
/// already there. So the `UPDATE` is refused, and so is every later credential write that mints a
/// `paper` row. SQLite has no `ALTER TABLE … DROP CONSTRAINT`; the documented cure is a rebuild,
/// which is what [`reshape_into`] already does for its own reason (*"the rebuild is a COPY,
/// because `ALTER TABLE` cannot do it"*). Rewriting the rows and replacing the constraint are
/// therefore the SAME act, and doing one without the other leaves a store that either refuses its
/// own vocabulary or holds a word nothing reads.
///
/// # What it does
///
/// For each of [`RETIER_TABLES`] whose `sqlite_master` entry still names the old word, it calls
/// [`rebuild_table_from_ddl`] with a SELECT rewrite that maps the value, then asserts the NEW
/// vocabulary is in the rebuilt constraint.
///
/// ⚠ **The four traps that rebuild is written around are documented THERE, not here** — this
/// function used to carry them and stage 4's `AUTOINCREMENT` needed the identical procedure, so
/// the body moved rather than being spelled a second time. Trap 4 is the one whose meaning changed
/// with that move: the mark carry was a no-op while no table declared `AUTOINCREMENT`, and
/// [`migrate_tables_onto_autoincrement`] is what made it load-bearing.
///
/// The transaction is the caller's, so a failure anywhere leaves the store exactly as it was.
///
/// # Errors
/// The engine.
pub(crate) fn migrate_sim_tier_to_paper(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    for table in RETIER_TABLES {
        let Some(sql) = table_sql(tx, table)? else { continue };
        if !sql.contains(&format!("'{SIM_TIER_WORD}'")) {
            continue;
        }
        // Traps 1–4 all live in [`rebuild_table_from_ddl`], which is the one spelling of this
        // procedure. The only thing peculiar to §4.4 is the SELECT rewrite below: `tier` is
        // rewritten IN the copy rather than afterwards, because an `UPDATE` after the copy would
        // run against the new CHECK the copy just passed.
        let rebuilt = rebuild_table_from_ddl(tx, table, RETIER_SCRATCH_SUFFIX, &|column| {
            (column == "tier").then(|| {
                format!("CASE tier WHEN '{SIM_TIER_WORD}' THEN '{PAPER_TIER}' ELSE tier END")
            })
        })?;
        if rebuilt.decline_note(table).is_some() {
            // ⚠ A DECLINE IS NOT NOTHING-TO-DO, and this `continue` is where the difference is
            // still lost. The store keeps its `'sim'` CHECK and refuses every later `paper` write
            // with nothing naming the cause. `Rebuild::decline_note` renders that cause and its
            // doc names the one change owed — a warnings channel out of `crate::db::
            // ensure_venue_id_columns`. ⚠ Do NOT "fix" this by returning an `Err`:
            // `required_columns_of`'s doc rules that out, and it would take the operator's
            // credential write down with the repair.
            continue;
        }

        // ⚠ The POSITIVE check, and it is the one this file's own programme insists on: assert the
        // NEW vocabulary is in the shipped constraint, never merely that the old one is gone. A
        // rebuild that produced a table with no CHECK at all would pass the second and fail this.
        let rebuilt = table_sql(tx, table)?.unwrap_or_default();
        if !rebuilt.contains(&format!("'{PAPER_TIER}'")) {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
                Some(format!(
                    "the rebuilt `{table}` does not constrain `tier` against '{PAPER_TIER}'; \
                     nothing was committed"
                )),
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// §4.1 — `AUTOINCREMENT`, and the uniform `id` shape (ruling 6)
// ---------------------------------------------------------------------------------------------

/// Suffix for the scratch name §4.1's rebuild builds the new shape under.
///
/// Distinct from [`RETIER_SCRATCH_SUFFIX`] deliberately: the two rebuilds run in the same
/// transaction on the same tables, and a shared scratch name would make a failure of one look like
/// a leftover of the other.
const AUTOINCREMENT_SCRATCH_SUFFIX: &str = "_pre_autoincrement";

/// **Carry an EXISTING store onto ruling 6's uniform `id` shape** — `id INTEGER PRIMARY KEY
/// AUTOINCREMENT` on every table [`DDL`] arms, and `node_key`'s surrogate `id` beside the `name`
/// that used to be its PRIMARY KEY. A no-op on a store born after the rebuild, and on any table
/// that already declares it.
///
/// # What it buys, in the spec's own words
///
/// `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md` §2.4: there WAS no
/// `AUTOINCREMENT` anywhere, so SQLite hands a new row `max(rowid) + 1`, and
/// [`crate::db::edit_account`]'s `DELETE` frees the top id — *"remove account 16, add an account,
/// and the new one IS account 16."* §4.1 rules the cure is the ENGINE's job rather than a number
/// computed in application code, and notes what makes it a REBUILD rather than an `ALTER`:
/// ⚠ **`AUTOINCREMENT` cannot be added by `ALTER TABLE` at all.** (The past tense is load-bearing
/// and was a present tense until this was swept: it describes the PRE-state of the store this
/// function repairs, which is what the heading and the summary above both frame, and stage 4a has
/// landed — that spec section's own heading now ends *"— CLOSED by stage 4a"*.)
///
/// # ⚠ Why this is not gated on [`crate::SCHEMA_VERSION`]
///
/// For the reason [`DDL`]'s own doc has recorded since the four post-freeze tables landed, and
/// which Task 5 re-ruled for `account.armed`: [`crate::READABLE_SCHEMA_VERSIONS`] is
/// `[1, SCHEMA_VERSION]`, so a bump to 3 silently DROPS 2 — the version both live boxes hold — and
/// their credential stores read as unreadable, which is every venue on paper with nothing
/// erroring. So the trigger is the SHAPE the store actually has, asked of `sqlite_master`, exactly
/// as [`migrate_sim_tier_to_paper`] asks whether the old tier word is still in the CHECK.
/// [`crate::ACCOUNT_TABLE_SCHEMA`] does not move either — it answers *is this store old enough
/// that the `account` table does not exist*, which an `id` column's shape does not change.
///
/// # ⚠ What makes the mark CORRECT on this particular rebuild
///
/// §4.1 again: *"On THIS migration there is no prior mark to lose (the constraint is being
/// introduced), so copying rows with their ids sets the sequence to `max(id)`, which is correct."*
/// [`rebuild_table_from_ddl`] reads the mark before and restores it after regardless, which is
/// `None` here and a no-op — and is the statement that stops being a no-op for every FUTURE
/// rebuild of these tables, including [`migrate_sim_tier_to_paper`]'s, now that they are armed.
/// `crates/vike-secrets/tests/sqlite_sequence_gate.rs` is the gate.
///
/// # ⚠ It no longer runs [`migrate_dropped_columns`], and the move is the point
///
/// §9 stage 4c's delivery pass used to be this function's closing statement, because this was the
/// one schema-owned entry `crate::db::ensure_venue_id_columns` already called and the task that
/// wrote it did not own `db.rs`. It now sits BESIDE this call in that funnel, one line after it,
/// which is where both docs always said it belonged. Same order, same behaviour — this function
/// has exactly one caller and that one had exactly one.
///
/// Worth keeping rather than deleting, because the nesting was actively misleading: the two
/// repairs have DIFFERENT triggers and different lifetimes. This one's goes false FOREVER once a
/// store has been carried, and it never visits `venue_arming` at all; that one's is *the store's
/// table still HAS the column*, which stays true afterwards. Nested, the second reads as a phase
/// of the first — and the next author who deletes this pass on the grounds that it is spent takes
/// the other one with it.
///
/// # Errors
/// The engine and [`rebuild_table_from_ddl`]'s own refusals.
pub(crate) fn migrate_tables_onto_autoincrement(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    for table in autoincrement_tables() {
        let Some(sql) = table_sql(tx, &table)? else { continue };
        if sql.to_uppercase().contains("AUTOINCREMENT") {
            continue;
        }
        // ⚠ The decline is NAMED rather than being a bare `continue` — see
        // `Rebuild::decline_note`, and the same ⚠ at `migrate_sim_tier_to_paper`'s call site.
        let rebuilt = rebuild_table_from_ddl(tx, &table, AUTOINCREMENT_SCRATCH_SUFFIX, &|_| None)?;
        if rebuilt.decline_note(&table).is_some() {
            continue;
        }

        // The POSITIVE check, the same one §4.4's rebuild performs against its own vocabulary:
        // assert the NEW shape is on the table, never merely that the old one is gone. A rebuild
        // that silently produced the old body would pass every other statement here.
        let rebuilt = table_sql(tx, &table)?.unwrap_or_default();
        if !rebuilt.to_uppercase().contains("AUTOINCREMENT") {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
                Some(format!(
                    "the rebuilt `{table}` does not declare AUTOINCREMENT, so its `id` is still \
                     reusable; nothing was committed"
                )),
            ));
        }
    }

    // ⚠ [`migrate_dropped_columns`] USED TO BE CALLED HERE and is now called one line after this
    // function in `crate::db::ensure_venue_id_columns`, which is the funnel both repairs belong to
    // and what both docs always named as its home. The ORDER is unchanged and must stay this way
    // round: a table the loop above rebuilt has already lost a dropped column through the
    // intersection, so that pass then finds nothing to do rather than rebuilding it twice.
    Ok(())
}

/// Every table the shipped [`DDL`] arms with `AUTOINCREMENT`, DERIVED from the batch rather than
/// written down — so a table that joins the uniform shape joins this migration by the same edit
/// that arms it, and one that leaves it stops being rebuilt without a second edit here.
///
/// ⚠ The two tables deliberately NOT in the answer are not in it because [`DDL`] does not arm them,
/// and each has its own reason recorded at its `CREATE TABLE`: `settings_adoption` is ruling 6's
/// one stated exception (*"a singleton whose `id` is a seal rather than a surrogate"*), and
/// `venue_arming` is a table §3 spells DELETED, which a surrogate key would be work spent on
/// something scheduled to go.
fn autoincrement_tables() -> Vec<String> {
    let mut out = Vec::new();
    for chunk in DDL.split("CREATE TABLE IF NOT EXISTS ").skip(1) {
        let name: String = chunk.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        let Some(end) = chunk.find(") STRICT") else { continue };
        if chunk[..end].to_uppercase().contains("AUTOINCREMENT") {
            out.push(name);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// §9 stage 4c — a column that LEFT the batch, delivered to a store that still carries it
// ---------------------------------------------------------------------------------------------

/// Suffix for the scratch name a table is rebuilt under when a dropped column is being taken off
/// it.
///
/// Distinct from the two above for the reason [`AUTOINCREMENT_SCRATCH_SUFFIX`] gives about
/// [`RETIER_SCRATCH_SUFFIX`]: all three rebuilds run in one transaction over overlapping tables,
/// and a shared scratch name makes a failure of one look like a leftover of another.
const DEAD_COLUMN_SCRATCH_SUFFIX: &str = "_pre_column_drop";

/// **Every column stage 4c took OUT of [`DDL`], as `(table, column, the measurement that proved it
/// dead)`.**
///
/// ⚠ **A row here is a licence to DESTROY data on a live box**, so the bar is the one §9 stage 4c
/// sets and [`DDL`]'s own *dead columns* section spells per candidate: **zero readers AND zero
/// writers across the whole tree**. A column whose only writer is [`write_rows`] is WRITE-ONLY and
/// does not qualify — it has a value on disk that nothing would put back.
///
/// The set is deliberately NOT derived by comparing the shipped batch against a store: that
/// comparison also answers YES for a column a NEWER binary added, and an older binary running that
/// rule would delete it on the next write. Only a column this batch deliberately dropped may be
/// dropped, so it is named.
///
/// This module's own `the_dropped_columns_are_absent_from_the_batch` is the anti-vacuity half:
/// every row must name a table [`DDL`] still declares and a column it does NOT, so a column put
/// back into the batch reddens rather than being silently deleted from every store on the next
/// write.
///
/// # ⚠ A row's `why` may not spell the name of the mirror writer, and that is a GATE
///
/// `crates/vike-ops/tests/settings_row_writer_gate.rs` pins **which files may write a settings
/// ROW**, and it measures by looking for that writer's identifier in the COMMENT-STRIPPED source
/// of every `src/` file. A doc comment is stripped; **a string literal is not**. So the
/// measurement behind the one row below is written HERE, in the doc, and the row itself carries the
/// short form — spelling the identifier inside the `why` string reports this file as an unpinned
/// writer of the live ceilings, which is the same shape as the rule against writing a whole
/// credential-key literal in a `src/` file. MEASURED: it did, on the first push.
///
/// **`venue_arming.notes`, re-measured 2026-09-23.** The mirror writer in
/// `crates/vike-secrets/src/settings.rs` INSERTs `(venue, venue_id, label, mode, max_exposure)`;
/// the three statements that READ that table select `(venue, label, mode[, max_exposure])`; and no
/// statement anywhere else in the workspace touches the table at all — it is confined to that one
/// file. The same writer DELETEs and re-inserts every row on each mirror, so there is no historical
/// value to lose either. It is also the one `notes` column §3 does not declare, because §3 spells
/// the whole TABLE deleted — which is why this column can go and its six siblings cannot.
const DROPPED_COLUMNS: [(&str, &str, &str); 1] = [(
    "venue_arming",
    "notes",
    "§2.7, re-measured 2026-09-23: zero writers, zero readers, and the table is confined to one \
     file — the measurement is at this table's own doc, which is where it can name the mirror \
     writer without this file being reported as one. §3 declares no such column, because it spells \
     the whole table deleted.",
)];

/// **Carry an EXISTING store onto a [`DDL`] that has DROPPED a column** — the delivery half of
/// §9 stage 4c, and a no-op on a store born after the drop.
///
/// # ⚠ Why this needs a trigger of its own rather than riding the rebuild above
///
/// [`rebuild_table_from_ddl`] copies the INTERSECTION of the two column sets (its trap 3), so a
/// dropped column is already taken off any table it rebuilds — **on the one occasion it rebuilds
/// it**. That is not a delivery path, for two independent reasons:
///
/// * [`migrate_tables_onto_autoincrement`]'s trigger is *this table does not yet declare
///   `AUTOINCREMENT`*, which goes false FOREVER after the first rebuild. A column dropped from the
///   batch afterwards reaches a store that has already been carried — both live boxes, the moment
///   stage 4a ships — never.
/// * It only visits [`autoincrement_tables`], and `venue_arming` is deliberately not one of them.
///
/// So the trigger here is the state that is still true: **the store's table still HAS the column**.
/// Idempotent by construction, and the rebuild that answers it is the same one, so this adds no
/// second spelling of the procedure and no second `DROP TABLE` for
/// `crates/vike-secrets/tests/sqlite_sequence_gate.rs`'s source scan to classify.
///
/// # ⚠ Where it is CALLED from
///
/// `crate::db::ensure_venue_id_columns` — the idempotent repair funnel every writer passes through
/// — one line after [`migrate_tables_onto_autoincrement`], BESIDE its two siblings rather than
/// nested inside one of them. It was nested there until 2026-09-23, because the task that wrote
/// this owned `schema.rs` and not `db.rs`, and both docs named this as the home to move it to. The
/// ORDER is the same either way and must stay after the autoincrement pass: a table that pass
/// rebuilds already loses the column through the intersection, and this one then finds nothing to
/// do rather than rebuilding it a second time.
///
/// Nesting was not merely untidy. The two triggers have different LIFETIMES — that one's goes
/// false forever once a store has been carried, this one's does not — so the nested reading makes
/// this pass look like a phase of that one, and the next author who deletes the outer pass as
/// spent takes this with it.
///
/// # Errors
/// The engine, [`rebuild_table_from_ddl`]'s own refusals, and a refusal when a rebuild ran and the
/// column survived it.
pub(crate) fn migrate_dropped_columns(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    for (table, column, _why) in DROPPED_COLUMNS {
        // `PRAGMA table_info` answers with NO ROWS for a table that is not there, so an absent
        // table is a skip without a second probe for it.
        if !table_columns(tx, table)?.iter().any(|have| have == column) {
            continue;
        }
        // ⚠ The decline is NAMED rather than being a bare `continue` — see
        // `Rebuild::decline_note`, and the same ⚠ at `migrate_sim_tier_to_paper`'s call site.
        let rebuilt = rebuild_table_from_ddl(tx, table, DEAD_COLUMN_SCRATCH_SUFFIX, &|_| None)?;
        if rebuilt.decline_note(table).is_some() {
            continue;
        }

        // The POSITIVE check both migrations above perform, asked of the NEW shape: assert the
        // column is gone from the table that now carries the real name, never merely that a
        // rebuild was attempted. A rebuild that silently produced the old body would pass
        // everything else here.
        if table_columns(tx, table)?.iter().any(|have| have == column) {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
                Some(format!(
                    "the rebuilt `{table}` still carries `{column}`, which the shipped DDL no \
                     longer declares; nothing was committed"
                )),
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The rebuild — ONE spelling, used by all three migrations above
// ---------------------------------------------------------------------------------------------

/// What [`rebuild_table_from_ddl`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Rebuild {
    /// The table now carries [`DDL`]'s shape, its old rows, and its indexes.
    Done,
    /// **Left exactly as it was**, because the store's table is missing a column the shipped shape
    /// declares `NOT NULL` with no `DEFAULT` — see [`required_columns_of`] for what that state is
    /// and why a skip is the right answer to it.
    ///
    /// ⚠ **The missing names ride along, and that is the whole point of the payload.** This variant
    /// was a unit, and the three call sites turned it into a bare `continue` — so a store that
    /// DECLINED the repair was indistinguishable from one that needed none. What the operator then
    /// meets is the next write refusing against the shape that was never repaired (a `'sim'` CHECK
    /// rejecting a `paper` tier, say) with nothing anywhere naming the cause. The columns are
    /// carried so the cause exists as data at the moment it is known rather than being computed
    /// and thrown away.
    ///
    /// # ⚠ NOTHING CAN REACH THIS VARIANT TODAY — stated plainly rather than implied
    ///
    /// No test constructs it through [`rebuild_table_from_ddl`]; the only coverage is
    /// `a_declined_rebuild_carries_the_reason_it_declined` in this module's own `schema_tests`,
    /// which builds the
    /// variant by hand, so what is proved is [`Rebuild::decline_note`]'s RENDERING and not the
    /// decline. And nothing in production can produce it, because [`required_columns_of`] derives
    /// its set from the SHIPPED [`DDL`] and every store that reaches this code was created by some
    /// version of that same `DDL`. The one older shape — schema 1 — dies EARLIER:
    /// `crate::db::ensure_venue_id_columns` opens with `crate::db`'s `ensure_venue_rows`, whose
    /// `tx.execute_batch(DDL)` cannot PREPARE `credential_one_live_value ON credential
    /// (account_id, field)` against it. MEASURED by the branch review of 2026-09-23 on a planted
    /// `account` with no `venue_account_id`: `no such column: venue_account_id`, out of that batch,
    /// with the rebuild log empty. That guard covers every required column an INDEX also names,
    /// which is most of them.
    ///
    /// **What would make it reachable**, and it is the state the variant exists for: `DDL` gaining
    /// a `NOT NULL`-with-no-`DEFAULT` column on an existing table. SQLite refuses to `ALTER TABLE
    /// … ADD COLUMN` one, so `DDL` alone cannot deliver it to a store already on disk, and on the
    /// day such a column ships every pre-existing store declines here. A column no index names
    /// reaches this branch rather than the `execute_batch` crash above it — `account.tier` and
    /// `credential.value` are two that are required and indexed by nothing.
    ///
    /// ⚠ **So the warnings channel [`Rebuild::decline_note`] records as OWED is owed for a branch
    /// nothing can reach**: it is not a hole in today's repair, and building it now would be
    /// building a reporting path with no producer. Build it with the first `DDL` change that can
    /// trigger a decline, and do not delete these branches in the meantime — they are the correct
    /// answer to a state this schema is one column away from.
    Skipped {
        /// Every `NOT NULL`-with-no-`DEFAULT` column the shipped shape declares and the store's
        /// table does not have — what [`Rebuild::decline_note`] renders.
        missing: Vec<String>,
    },
}

impl Rebuild {
    /// The operator-facing sentence for a decline, or `None` for a rebuild that ran.
    ///
    /// ⚠ **This crate has NOWHERE to send it yet, and that is a declared residual rather than an
    /// oversight.** `vike-secrets` carries no logging dependency by design (one external crate,
    /// `rusqlite`), and the three passes that call [`rebuild_table_from_ddl`] return
    /// `rusqlite::Result<()>` into `crate::db::ensure_venue_id_columns`, inside the caller's
    /// transaction. Turning a decline into a REFUSAL is ruled out on its own terms:
    /// [`required_columns_of`]'s doc states the design — *a repair that cannot run must decline
    /// rather than abort* — and a refusal there would take out the operator's credential write.
    /// So what is owed is a warnings channel out through that funnel, which is one signature in
    /// `crate::db`. Until it exists, this renders the note and the call sites name it.
    ///
    /// ⚠ **Read that debt with [`Rebuild::Skipped`]'s own reachability section beside it**: no
    /// store this binary can meet produces a decline today, so the channel is owed for a producer
    /// that does not exist yet. That is a reason to leave it unbuilt, not a reason to delete the
    /// renderer — the first `NOT NULL`-with-no-`DEFAULT` column `DDL` gains makes both real on the
    /// same day.
    fn decline_note(&self, table: &str) -> Option<String> {
        let Rebuild::Skipped { missing } = self else { return None };
        Some(format!(
            "`{table}` was left on its old shape: the store's table has no {}, which the shipped \
             DDL declares NOT NULL with no DEFAULT, so a rebuild would have nothing to put there. \
             Every later write this repair was meant to enable will be refused by the OLD \
             constraint until that column exists",
            missing.join(", ")
        ))
    }
}

/// **Rebuild `table` into [`DDL`]'s shape, carrying its rows, its ids and its `AUTOINCREMENT`
/// high-water mark** — the one spelling of the procedure both [`migrate_sim_tier_to_paper`] and
/// [`migrate_tables_onto_autoincrement`] perform.
///
/// `select_expr` rewrites ONE column's value during the copy, returning `None` for a column that
/// travels unchanged. It exists because a value migration and a shape migration are the same act:
/// the rewrite must happen IN the `INSERT … SELECT`, since an `UPDATE` afterwards would run
/// against the new constraint the copy just passed.
///
/// # The four traps this is written around
///
/// 1. **The OLD table keeps the real name; the NEW one is built beside it and renamed in.** The
///    obvious spelling is the one [`reshape_into`] uses — rename the old table aside, re-run
///    [`DDL`], copy, drop — and it is WRONG for `account`, MEASURED rather than reasoned about.
///    `crates/vike-secrets/src/db.rs`'s open path sets and VERIFIES `PRAGMA foreign_keys = ON`,
///    and under that setting SQLite REWRITES every other table's `REFERENCES` clause to follow a
///    rename: `credential.account_id REFERENCES account(id)` silently becomes `REFERENCES
///    account_pre_paper_tier(id)` and survives the scratch table's own DROP as a clause pointing
///    at nothing. ⚠ **`PRAGMA legacy_alter_table` does NOT suppress that HERE, and the reason is
///    not the one this line used to give.** It said *it governs trigger and view bodies, while the
///    foreign-key rewrite is governed by `foreign_keys`*, and that mechanism is wrong — MEASURED on
///    2026-09-23 against SQLite 3.53.2, the matrix is in
///    `crates/vike-secrets/tests/sqlite_sequence_gate.rs`'s `rebuild_preserving_ids`. What the
///    engine actually does is: with `foreign_keys` OFF, `legacy_alter_table = ON` DOES suppress the
///    `REFERENCES` rewrite; with `foreign_keys` ON, nothing suppresses it. The conclusion for THIS
///    function is unchanged and is now load-bearing for the right reason — `PRAGMA foreign_keys` is
///    a NO-OP inside a transaction (measured: issued inside one it returns `Ok` and the engine
///    still answers `1`), so every caller here runs with foreign keys ON, which is exactly the
///    corner where `legacy_alter_table` cannot help. SQLite's own 12-step procedure ("turn the FKs
///    off for the rebuild") is unavailable for the same reason. The first draft of this used
///    `legacy_alter_table` and this function's own `pragma_foreign_key_check` refused it, naming
///    two dangling `credential` rows. Building the new table under the scratch name inverts the
///    problem away: the only rename left is `scratch -> {table}`, and NOTHING references the
///    scratch name, so no clause is rewritten and `credential` never stops naming `account`.
/// 2. **The indexes go with the dropped table, and the closing [`DDL`] pass is what puts them
///    back.** `account_one_account_per_book` and `venue_setting`'s two partial indexes belong to
///    the table being dropped, so their names are free again and `CREATE … IF NOT EXISTS`
///    re-creates each one on the table that now carries the real name. (Under the rename-aside
///    spelling this was a trap instead: an index follows a RENAME, keeping its name, so the
///    `IF NOT EXISTS` found the name taken, created nothing, and the drop then took the index.)
/// 3. **The column set is INTERSECTED, not assumed.** `venue_id` and `armed` reach an existing
///    store through `ALTER TABLE` (see [`DDL`]'s own doc), so the old table's columns are a subset
///    of the new one's on some stores and — for a store written by a NEWER binary and opened by
///    this one — could be a superset. `INSERT … SELECT` over the intersection needs no version
///    arithmetic. ⚠ **It is not equally CORRECT in the two directions, and this line used to claim
///    it was.** For the SUBSET direction it is correct: the columns the old table lacks are ones
///    the shipped shape can fill or leave null. For the SUPERSET direction it is silent DATA LOSS —
///    a column only the NEWER binary knows about is dropped on the floor by an older binary's
///    repair, with nothing said. No guard is built for it, deliberately: no column and no index has
///    ever been REMOVED from [`DDL`] (the whole history was read), so the superset case has never
///    occurred, and a guard over a shape nobody has produced would be a refusal written against a
///    guess. What this note buys is that whoever first removes a column knows they are the one
///    making it reachable. Any `id` the old table has is copied EXPLICITLY, so no row changes
///    identity and `credential.account_id` still resolves; a table that never had one (`node_key`
///    before ruling 6's uniform shape) is simply handed fresh ids by the engine.
/// 4. **The `AUTOINCREMENT` high-water mark is CARRIED.** §4.1's hazard: the mark lives in a row of
///    `sqlite_sequence`, a dropped table takes its row with it, and a rebuild replaying the
///    SURVIVING rows with their ids leaves the mark at `max(id)` of what it replayed — a REWIND
///    whenever the top row had been removed, after which an id is REUSED and every reference to
///    the dead account silently names a live one. ⚠ **That carry stopped being a no-op the day
///    [`DDL`] armed these tables**, which is the same change that introduced
///    [`migrate_tables_onto_autoincrement`]; `crates/vike-secrets/tests/sqlite_sequence_gate.rs`'s
///    `the_paper_tier_rebuild_does_not_rewind_the_account_marks` is the behaviour assertion that
///    now fails without it.
///
/// The transaction is the caller's, so a failure anywhere leaves the store exactly as it was.
///
/// # Errors
/// The engine, and a refusal when the rebuild left a dangling reference.
fn rebuild_table_from_ddl(
    tx: &Transaction<'_>,
    table: &str,
    scratch_suffix: &str,
    select_expr: &dyn Fn(&str) -> Option<String>,
) -> rusqlite::Result<Rebuild> {
    let columns = table_columns(tx, table)?;
    let missing: Vec<String> =
        required_columns_of(table).into_iter().filter(|need| !columns.contains(need)).collect();
    if !missing.is_empty() {
        return Ok(Rebuild::Skipped { missing });
    }

    let scratch = format!("{table}{scratch_suffix}");
    // Trap 4 — read BEFORE the drop, restored after the replay.
    let mark = autoincrement_mark(tx, table)?;

    // Trap 1 — the NEW table is built beside the old one under a scratch name, and the OLD one
    // keeps the real name until it is dropped.
    //
    // `defer_foreign_keys` is what makes the window legal: `DROP TABLE {table}` below performs an
    // implicit `DELETE FROM`, which fires every child row's foreign key, and the parent is only
    // put back by the rename on the next line. Deferring moves enforcement to COMMIT — and this
    // function re-checks the WHOLE database itself before returning, so nothing is merely
    // postponed. It is turned back OFF at the end, so the caller's transaction is left enforcing
    // exactly what it was.
    tx.execute_batch("PRAGMA defer_foreign_keys = ON;")?;
    // ⚠ A LEFTOVER SCRATCH TABLE IS OTHERWISE AN UNRECOVERABLE DEAD END, and this one statement is
    // the whole cure. `create_statement_under` spells a bare `CREATE TABLE`, not
    // `CREATE TABLE IF NOT EXISTS` — deliberately, because `IF NOT EXISTS` would silently REUSE a
    // stale scratch of the wrong shape and copy the rows into it. So without this drop, a scratch
    // table that ever survived would make every future credential write on that box refuse forever
    // with a bare `table account_pre_autoincrement already exists`: an error naming nothing an
    // operator can act on, on the binary that holds their venue keys. This rebuild is atomic (the
    // caller's transaction), so nothing in THIS code can leave one behind — the statement is here
    // for the states this code did not produce, which is the only kind a repair path ever meets.
    tx.execute_batch(&format!("DROP TABLE IF EXISTS {scratch};"))?;
    tx.execute_batch(&create_statement_under(table, &scratch)?)?;

    // Trap 3 — the intersection, with the caller's rewrite applied in the SELECT.
    let fresh = table_columns(tx, &scratch)?;
    let carried: Vec<&String> = columns.iter().filter(|c| fresh.contains(c)).collect();
    let selected: Vec<String> =
        carried.iter().map(|c| select_expr(c).unwrap_or_else(|| (*c).clone())).collect();
    let names: Vec<&str> = carried.iter().map(|c| c.as_str()).collect();
    tx.execute_batch(&format!(
        "INSERT INTO {scratch} ({}) SELECT {} FROM {table};",
        names.join(", "),
        selected.join(", ")
    ))?;

    // Trap 2 — the old table's NAMED INDEXES go with it, which is what makes the final `DDL` pass
    // necessary rather than decorative.
    //
    // ⚠ The drop is its OWN statement and its own call. `crates/vike-secrets/tests/
    // sqlite_sequence_gate.rs`'s `table_drops` scan classifies every table drop this crate's
    // source performs, keyed on the token that follows `DROP TABLE`, and `TABLE_DROP_PIN` carries
    // this one's row. Keeping the statement alone keeps the token the pin names readable here.
    tx.execute_batch(&format!("DROP TABLE {table};"))?;
    tx.execute_batch(&format!("ALTER TABLE {scratch} RENAME TO {table};"))?;
    tx.execute_batch(DDL)?;

    // Trap 4's other half: the replay above set the mark to `max(id)` OF WHAT IT REPLAYED, which
    // is a REWIND whenever the top row had been removed. Put back what was there.
    if let Some(seq) = mark {
        tx.execute("DELETE FROM sqlite_sequence WHERE name = ?1", [table])?;
        tx.execute("INSERT INTO sqlite_sequence (name, seq) VALUES (?1, ?2)", (table, seq))?;
    }
    tx.execute_batch("PRAGMA defer_foreign_keys = OFF;")?;

    // Deferring enforcement is not skipping it — this is [`reshape_into`]'s own closing move, and
    // it asks about the WHOLE database rather than about the statements just run.
    let violations: i64 =
        tx.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get(0))?;
    if violations > 0 {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some(format!(
                "rebuilding `{table}` from the shipped DDL left {violations} dangling \
                 reference(s); nothing was committed"
            )),
        ));
    }
    Ok(Rebuild::Done)
}

/// **Every column [`DDL`]'s `table` declares `NOT NULL` with no `DEFAULT`** — the columns a rebuild
/// must be able to fill FROM THE OLD TABLE, because the copy is over the intersection and a column
/// the old table does not have is handed nothing at all.
///
/// ⚠ **This guard has exactly one reachable subject and it is the one that matters: a schema-1
/// `credential`.** That table is `(name TEXT PRIMARY KEY, value TEXT)` — no `field` — so a rebuild
/// into the shipped shape would `INSERT` rows whose `field` is NULL against a `NOT NULL` column
/// and abort the operator's whole write. [`crate::db::fill_into`] runs [`reshape_into`] before it
/// reaches here, and the four other callers of [`crate::db::ensure_venue_id_columns`] already fail
/// on such a store for a separate PRE-EXISTING reason (their own `DDL` batch's
/// `credential_one_live_value` index, `no such column: account_id`) — so nothing reaches this
/// guard today. It is written because the alternative is a repair step whose failure mode on an
/// old store is *the credential write was refused*, and a repair that cannot run must decline
/// rather than abort.
///
/// It is DERIVED from the shipped batch rather than written down, so a new `NOT NULL` column is
/// covered by the edit that adds it.
fn required_columns_of(table: &str) -> Vec<String> {
    ddl_column_decls(table)
        .into_iter()
        .filter(|(_, decl)| decl.contains("NOT NULL") && !decl.contains("DEFAULT"))
        .map(|(name, _)| name)
        .collect()
}

/// `(column name, the rest of its declaration)` for every COLUMN of [`DDL`]'s `table`.
///
/// Table-level constraints are NOT columns and are dropped: an entry whose first word is one of
/// [`TABLE_CONSTRAINT_WORDS`] is skipped. Entries are split on commas at PAREN DEPTH ZERO, so the
/// commas inside `CHECK (tier IN ('paper', 'demo', 'live'))` and `UNIQUE (venue, tier, label)` do
/// not split anything.
fn ddl_column_decls(table: &str) -> Vec<(String, String)> {
    let head = format!("CREATE TABLE IF NOT EXISTS {table} (");
    let Some(at) = DDL.find(&head) else { return Vec::new() };
    let rest = &DDL[at + head.len()..];
    let Some(end) = rest.find(") STRICT") else { return Vec::new() };

    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut entry = String::new();
    for c in rest[..end].chars() {
        match c {
            '(' => {
                depth += 1;
                entry.push(c);
            }
            ')' => {
                depth = depth.saturating_sub(1);
                entry.push(c);
            }
            ',' if depth == 0 => {
                push_column_decl(&mut out, &entry);
                entry.clear();
            }
            _ => entry.push(c),
        }
    }
    push_column_decl(&mut out, &entry);
    out
}

/// The words that open a TABLE-level constraint rather than a column.
const TABLE_CONSTRAINT_WORDS: [&str; 5] = ["CHECK", "UNIQUE", "PRIMARY", "FOREIGN", "CONSTRAINT"];

fn push_column_decl(out: &mut Vec<(String, String)>, entry: &str) {
    let entry = entry.trim();
    let Some((name, decl)) = entry.split_once(char::is_whitespace) else { return };
    if TABLE_CONSTRAINT_WORDS.contains(&name.to_uppercase().as_str()) {
        return;
    }
    out.push((name.to_string(), decl.trim().to_string()));
}

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
///   screen: `crates/vike-app-core/src/tool_views/venues.rs`'s `credentials_cell` renders it
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
const SIM_TIER_WORD: &str = "sim";

/// One table's `CREATE TABLE` text as the engine stored it, or `None` when the table is absent.
fn table_sql(tx: &Transaction<'_>, table: &str) -> rusqlite::Result<Option<String>> {
    tx.query_row("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1", [table], |r| {
        r.get::<_, Option<String>>(0)
    })
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(other),
    })
}

/// One table's column names, in declaration order.
fn table_columns(tx: &Transaction<'_>, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = tx.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    rows.collect()
}

/// **[`DDL`]'s `CREATE TABLE` for `table`, re-pointed at `under`** — the statement that builds the
/// new shape beside the old one.
///
/// The head is rewritten and the BODY is taken verbatim, so the rebuilt table is [`DDL`]'s own
/// definition rather than a second spelling of it: a column added to the batch reaches a migrated
/// store through this function with no edit here. A head that cannot be found is an ERROR rather
/// than a skip — it would mean this function and [`RETIER_TABLES`] disagree about what the batch
/// contains, and the quiet version of that is a store left on the old vocabulary.
fn create_statement_under(table: &str, under: &str) -> rusqlite::Result<String> {
    let head = format!("CREATE TABLE IF NOT EXISTS {table} (");
    let Some(at) = DDL.find(&head) else {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some(format!("the shipped DDL declares no `{head}…` to rebuild `{table}` from")),
        ));
    };
    let rest = &DDL[at + head.len()..];
    let Some(end) = rest.find(";") else {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some(format!("the shipped DDL's `{table}` statement is unterminated")),
        ));
    };
    Ok(format!("CREATE TABLE {under} ({};", &rest[..end]))
}

/// One table's `AUTOINCREMENT` high-water mark, or `None` when it declares none.
///
/// `sqlite_sequence` does not exist at all in a database with no `AUTOINCREMENT` table anywhere,
/// so its absence is asked of `sqlite_master` first rather than allowed to arrive as a `no such
/// table` error that would be indistinguishable from a real one.
fn autoincrement_mark(tx: &Transaction<'_>, table: &str) -> rusqlite::Result<Option<i64>> {
    let present: i64 = tx.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'sqlite_sequence'",
        [],
        |r| r.get(0),
    )?;
    if present == 0 {
        return Ok(None);
    }
    tx.query_row("SELECT seq FROM sqlite_sequence WHERE name = ?1", [table], |r| r.get(0))
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
}

// ---------------------------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------------------------

/// One reason a schema-2 write refused. **Names a KEY, never a value.**
///
/// Same contract as [`crate::Ambiguity`], and for the same reason: an operator has to be able to
/// read a refusal out of a log without the log becoming a credential.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SchemaRefusal {
    /// The classifier returned a `field` that is not a suffix of the NAME **for a row whose
    /// account can ONLY be found by its owner prefix** (`Classification::needs_owner_prefix` —
    /// unlabelled, and discriminated, i.e. dukascopy's shape), so §4.4's derivation (*the name with
    /// its owner prefix removed*) did not hold and there is no second way to reach the account.
    /// A classifier bug, refused rather than papered over with a guessed prefix.
    ///
    /// ⚠ **It is NOT raised for every non-suffix `field`, and this doc said it was.** A LABELLED
    /// key's `field` is legitimately not a suffix of its name (the `__LABEL` sits after it), so a
    /// blanket refusal would refuse the whole labelled grammar; every other row survives a missing
    /// prefix because `(venue, tier, label)` still answers for it. See
    /// [`Classification::owner_prefix`], whose own doc carried the same overstatement.
    FieldIsNotASuffix {
        /// The key name.
        key: String,
        /// What the classifier said `field` was.
        field: String,
    },
    /// The classifier named a tier outside [`ACCOUNT_TIERS`]. Refused HERE rather than left to the
    /// `CHECK`, so the message names the KEY rather than an opaque constraint failure.
    UnknownTier {
        /// The key name.
        key: String,
        /// The tier the classifier answered.
        tier: String,
    },
    /// More than one unlabelled `account` row already exists for a `(venue, tier)` the classifier
    /// offered no discriminator for, so *which account is this key's?* has more than one answer and
    /// nothing here may pick. See [`AccountKey::discriminator`].
    AmbiguousAccount {
        /// The key name.
        key: String,
        /// The venue.
        venue: String,
        /// The tier.
        tier: String,
    },
    /// A commented-out `#KEY=VALUE` line names a key with no LIVE row in this store, so there is
    /// nothing it can be the superseded value OF. The line is REPORTED and NOT written — see
    /// [`FileComments`].
    SupersededKeyIsNotInTheStore {
        /// The key name. Never the commented value.
        key: String,
    },
    /// **Two credential NAMES resolve to one `(account_id, field)` and their VALUES DISAGREE.**
    ///
    /// The live case is a tier ALIAS: `vike_model::account_keys` normalizes the legacy `MAINNET`
    /// tier onto `LIVE`, so `{VENUE}_LIVE_API_KEY` and `{VENUE}_MAINNET_API_KEY` classify to the
    /// same account and — the store's own tier token being what §4.4 removes — to the same `field`.
    /// One credential, two spellings. `credential_one_live_value` says a live `(account, field)`
    /// has ONE value, and when the two spellings carry DIFFERENT values there is no answer here
    /// that is not a guess: whichever row were made live would silently decide which key a venue
    /// signs orders with.
    ///
    /// So neither is written, BOTH names ride the refusal, and every other key in the run lands.
    /// The repair is an operator edit — delete one of the two lines — which is exactly the shape
    /// every other per-key refusal in this module takes. When the two values are IDENTICAL nothing
    /// is refused at all: see [`RowReport::aliases`].
    CollidingLiveValues {
        /// The key name this row was being written for. Never its value.
        key: String,
        /// The name already holding the live row for the same `(account_id, field)`. Never its
        /// value.
        other: String,
        /// The `field` both of them derive.
        field: String,
    },
}

impl SchemaRefusal {
    /// The key NAME this refusal is about. Every variant has one; none of them has a value.
    #[must_use]
    pub fn key(&self) -> &str {
        match self {
            SchemaRefusal::FieldIsNotASuffix { key, .. }
            | SchemaRefusal::UnknownTier { key, .. }
            | SchemaRefusal::AmbiguousAccount { key, .. }
            | SchemaRefusal::SupersededKeyIsNotInTheStore { key }
            | SchemaRefusal::CollidingLiveValues { key, .. } => key,
        }
    }
}

impl std::fmt::Display for SchemaRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchemaRefusal::FieldIsNotASuffix { key, field } => write!(
                f,
                "{key}: the classifier answered field {field:?}, which is not the end of that \
                 name — the owner prefix a schema-2 row is keyed on cannot be taken from it, and \
                 nothing here will guess one"
            ),
            SchemaRefusal::UnknownTier { key, tier } => write!(
                f,
                "{key}: the classifier answered tier {tier:?}, which is not one of {:?} — an \
                 account row carrying it could not be joined to a venue arming ceiling",
                ACCOUNT_TIERS
            ),
            SchemaRefusal::AmbiguousAccount { key, venue, tier } => write!(
                f,
                "{key}: more than one unlabelled {venue} account already exists at tier {tier} and \
                 the classifier offered no discriminator, so which one this key belongs to has \
                 more than one answer — nothing here will pick"
            ),
            SchemaRefusal::SupersededKeyIsNotInTheStore { key } => write!(
                f,
                "{key} appears as a commented-out assignment in the credential file but has no \
                 live row in this store, so it is not the superseded value OF anything — the line \
                 is reported and nothing was written from it"
            ),
            SchemaRefusal::CollidingLiveValues { key, other, field } => write!(
                f,
                "{key} and {other} are two spellings of ONE credential — they resolve to the same \
                 account and the same field {field:?} — and they carry DIFFERENT values. A live \
                 (account, field) has one value, and nothing here will pick which of the two a \
                 venue signs orders with. NEITHER was written; delete one of the two lines and \
                 run again"
            ),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The comment scan — spec 4.2 and 4.3
// ---------------------------------------------------------------------------------------------

/// **What the credential FILE's comment lines carry** — the one thing `parse_dotenv` throws away.
///
/// ⚠ **This is a SECOND read of the credential file, and the property that makes it safe is that it
/// produces no LIVE value.** [`crate::db::migrate`]'s step 1 reads through `crate::store::resolve`
/// — *"the one parser, so the migration cannot disagree with the reader about what a line means"* —
/// and `crate::dotenv::parse_dotenv` skips every line beginning `#`. §4.2's two superseded
/// `ASTER_*` values and §4.3's provenance comments are therefore unreachable through it. This scan
/// looks ONLY at lines the one parser skipped, so the two can never disagree about anything that
/// reaches the credential map: this one contributes `superseded_at IS NOT NULL` rows and `notes`,
/// and no live row at all.
///
/// The file is opened READ-ONLY and is never written, moved or normalised — the rule that outranks
/// everything in `crate::db`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileComments {
    /// `#KEY=VALUE` lines — §4.2's rollback copies, as `(name, value)`. The value is a CREDENTIAL
    /// and is never printed by anything in this crate.
    pub superseded: Vec<(String, String)>,
    /// Prose comment blocks, keyed by the `KEY=` line they sit immediately above — §4.3's
    /// provenance. **Nothing ever reads these back**; they exist so that retiring the file does not
    /// destroy what the operator wrote in it.
    pub notes: BTreeMap<String, String>,
    /// Prose comment lines that sit above no key (a file header, a trailing note, anything
    /// separated from the next key by a blank line). COUNTED and never attached, because a note
    /// attached to the wrong row is worse than one nobody kept.
    pub unattached_prose_lines: usize,
}

/// Read [`FileComments`] out of a credential file's TEXT.
///
/// The rules, stated because §11 step 5 says a line is *classified, never guessed at*:
///
/// * a line matching `#[ ]*KEY=VALUE` (optional spaces after the `#`, a legal env-var name, an `=`)
///   is a SUPERSEDED VALUE;
/// * any other `#` line is PROSE;
/// * a run of prose lines **immediately** above a `KEY=` line — no blank line and no superseded
///   line between — is that key's note. Anything else is unattached and COUNTED.
///
/// ⚠ A `#` inside a VALUE is not a comment: this scan only considers lines whose first
/// non-whitespace byte is `#`, which is exactly `crate::dotenv::parse_dotenv`'s own skip condition,
/// so the two agree about which lines are comments by construction.
#[must_use]
pub fn scan_comments(text: &str) -> FileComments {
    let mut out = FileComments::default();
    let mut block: Vec<&str> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            out.unattached_prose_lines += block.len();
            block.clear();
            continue;
        }
        if let Some(body) = line.strip_prefix('#') {
            let body = body.trim_start();
            match split_assignment(body) {
                Some((name, value)) => {
                    // ⚠ **A `#KEY=VALUE` line ENDS the prose block**, and it did not until a
                    // reviewer measured §4.2's own file shape. The provenance line an operator
                    // writes above a rollback copy —
                    // `# superseded 2026-07-29 (kept for rollback)` — was left in the block, so it
                    // was carried PAST the rollback line and attached to the NEXT key in the file,
                    // whose note then claimed a rollback that was somebody else's. That is exactly
                    // what [`FileComments::unattached_prose_lines`] exists to prevent, and this
                    // function's own doc says a note is attached only to a key it sits
                    // IMMEDIATELY above.
                    //
                    // The block is COUNTED rather than attached because [`FileComments::superseded`]
                    // has no note column to put it in: a rollback copy carries the same `name` as
                    // the live row, so filing its provenance under that name would overwrite the
                    // live row's own note.
                    out.unattached_prose_lines += block.len();
                    block.clear();
                    out.superseded.push((name, value));
                }
                None => block.push(raw.trim_end()),
            }
            continue;
        }
        match split_assignment(line) {
            Some((name, _)) if !block.is_empty() => {
                out.notes.insert(name, block.join("\n"));
                block.clear();
            }
            _ => {
                out.unattached_prose_lines += block.len();
                block.clear();
            }
        }
    }
    out.unattached_prose_lines += block.len();
    out
}

/// `NAME=VALUE` with a legal credential-key name, or `None`.
///
/// The name grammar is deliberately the narrow one — an ASCII **UPPERCASE** letter or `_`, then
/// uppercase letters, digits and `_` — so a prose comment that merely contains an `=` is prose and
/// not a mangled superseded credential.
///
/// ⚠ **The uppercase requirement is a FENCE, and the example this doc used to give for it was
/// wrong.** It read: §4.3 quotes a real comment from the live store — *"polydata.live: key VALID
/// but FREE tier => data_access_days=0"* — and *"with a case-insensitive name grammar that tail
/// parses as an assignment and the line is read as a superseded value of a key called
/// `data_access_days`"*, described as MEASURED. It is not, and it cannot be: [`str::split_once`]
/// takes the FIRST `=`, which in that line is the one inside `=>`, so the candidate name is
/// `polydata.live: key VALID but FREE tier` — spaces, a dot and a colon — and no grammar that
/// admits an env-var name admits it, in any case. That line was never at risk and a
/// case-insensitive grammar would not have mis-read it.
///
/// What the rule actually fences is a comment whose WHOLE BODY is a lowercase assignment —
/// `# data_access_days=0`, the note left behind when somebody pastes the tail of that same sentence
/// onto its own line. THAT parses cleanly as `data_access_days = 0` under a case-insensitive
/// grammar and is read as a superseded credential. The `SupersededKeyIsNotInTheStore` safety net
/// catches it — there is no live row by that name, so nothing is written — but it catches it as a
/// REFUSAL an operator then has to read and dismiss on every run. Every credential key this store
/// holds is uppercase; a lowercase left-hand side is prose.
fn split_assignment(line: &str) -> Option<(String, String)> {
    let (name, value) = line.split_once('=')?;
    let name = name.trim();
    let mut bytes = name.bytes();
    let first = bytes.next()?;
    if !(first.is_ascii_uppercase() || first == b'_') {
        return None;
    }
    if !bytes.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_') {
        return None;
    }
    Some((name.to_string(), value.trim().to_string()))
}

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
/// 2. **By `(venue, tier, label)`.** The last resort, and it is what unifies a LEGACY tier spelling
///    with its canonical one: `ASTER_MAINNET_API_KEY` and `ASTER_LIVE_API_KEY` have DIFFERENT owner
///    prefixes and are one account, because `AccountRef::tier` normalizes `MAINNET` onto `LIVE`.
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
pub struct AccountResolver {
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
    created: Vec<(i64, String, String)>,
}

impl AccountResolver {
    /// Build the resolver from what the database already holds.
    ///
    /// Reads every live credential row's `(name, field, account_id)` and reconstructs each
    /// account's owner prefix from it, which is the state lookup 2 rests on.
    fn load(tx: &Transaction<'_>) -> rusqlite::Result<AccountResolver> {
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
            let mut stmt = tx.prepare("SELECT id, venue, tier, label FROM account")?;
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
    fn resolve(
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
        tx.execute(
            "INSERT INTO account (venue, venue_id, tier, label, notes) \
             VALUES (?1, (SELECT id FROM venue WHERE name = ?1), ?2, ?3, ?4)",
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
enum ResolveError {
    Sql(rusqlite::Error),
    Refused(SchemaRefusal),
}

// ---------------------------------------------------------------------------------------------
// Writing classified rows
// ---------------------------------------------------------------------------------------------

/// **What a schema-2 fill DID** — counts and NAMES, never a value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowReport {
    /// `account` rows this run created, as `(id, venue, tier)`.
    ///
    /// ⚠ The `id` is carried because `(venue, tier)` is NOT a key: dukascopy has TWO accounts at
    /// `(dukascopy, demo)` and neither carries a label, so a report keyed on the pair alone would
    /// fold ruling 1's whole point into one row — which it did, until a test said so. The id is the
    /// identity the owner's signature names, and it is not a secret.
    pub accounts_created: Vec<(i64, String, String)>,
    /// `credential` rows INSERTED with `superseded_at IS NULL`.
    ///
    /// ⚠ It counts INSERTS, and an ALIAS row is not one of them — it is inserted
    /// `superseded_at IS NOT NULL` by construction and counts under [`RowReport::alias_rows`]. The
    /// two together are the rows a fill CARRIED, which is what [`reshape_into`]'s guard compares.
    /// A row this fill inserted live and then DEMOTED (the canonical spelling arriving after its
    /// alias) is counted here, once, because that is what the fill did to it; the demotion is an
    /// `UPDATE` and is counted by neither field.
    pub live_rows: usize,
    /// `credential` rows INSERTED as the rollback copy of a name already holding the live row —
    /// see [`RowReport::aliases`], which NAMES them. The other half of *rows carried*.
    pub alias_rows: usize,
    /// **Two NAMES of one credential, as `(the alias, the name holding the live row)`.**
    ///
    /// Sorted, and never a value. The live case is the legacy tier spelling: a store holding both
    /// `{VENUE}_LIVE_API_KEY` and `{VENUE}_MAINNET_API_KEY` holds ONE credential under two names,
    /// because `vike_model::account_keys` normalizes `MAINNET` onto `LIVE` and §4.4 removes the
    /// store's own tier token from `field`. `credential_one_live_value` then admits exactly one of
    /// the two as live, so the other is filed `superseded_at IS NOT NULL` — which is what that
    /// column is for (§4.2: a rollback copy and the value that replaced it coexist) and is the
    /// only disposition that keeps BOTH `name` rows.
    ///
    /// Keeping both is the compatibility contract rather than tidiness: `crate::db::read_table`
    /// answers for an aliased name out of its superseded row (no live row carries it), so
    /// `crate::resolve_project` returns the same map before and after the upgrade and
    /// `vike-cli secrets list` still prints the operator's own spelling.
    ///
    /// ⚠ This is the IDENTICAL-value case. Two spellings carrying DIFFERENT values are refused by
    /// name instead — [`SchemaRefusal::CollidingLiveValues`].
    pub aliases: Vec<(String, String)>,
    /// **The key NAMES this fill WROTE**, sorted — live rows and superseded ones alike.
    ///
    /// ⚠ It exists because a SCHEMA UPGRADE inserts every row in the store while INSERTING NO NEW
    /// KEY, so a caller that records what a migration did from the pending set alone would journal
    /// nothing at all for the one act that rewrites the whole table and cannot be undone.
    ///
    /// ⚠ **`crate::db::migrate` performs that fold, into [`crate::Migration::inserted_keys`]** —
    /// this doc named `crates/vike-cli/src/cmd/secrets.rs`'s `record_migration` as the folder,
    /// which it is not: that function READS `inserted_keys` and hands the names to the change
    /// journal, and by the time it sees a `Migration` the fold has already happened. The
    /// distinction matters because the fold is what makes the CLI's guard (`if
    /// done.inserted_keys.is_empty() { return }`) record an upgrade at all; a reader who believed
    /// the CLI owned it would look for the bug in the wrong crate.
    pub written_names: Vec<String>,
    /// Superseded `credential` rows written from §4.2's commented-out assignments, by NAME.
    pub superseded_rows: Vec<String>,
    /// `notes` attached from §4.3's prose comments.
    pub notes_attached: usize,
    /// Comment lines that sat above no key and were therefore attached to nothing.
    pub unattached_prose_lines: usize,
    /// **Names the classifier could not place** — written verbatim as infrastructure rows and
    /// reported here (§11.1). Not an error; the store legitimately holds names no venue grammar
    /// covers.
    pub unrecognised: Vec<String>,
    /// **Rows §7 and §6 will move and this change deliberately did not** — by name, with the
    /// section that will take them. This is the next change's work-list.
    pub pending_moves: Vec<(String, PendingMove)>,
    /// Per-KEY refusals. The run still succeeded and every other row landed — the same disposition
    /// `crate::Ambiguity::DisagreesWithDatabase` already takes.
    pub refused: Vec<SchemaRefusal>,
}

impl RowReport {
    fn sort(&mut self) {
        self.accounts_created.sort();
        self.aliases.sort();
        self.aliases.dedup();
        self.superseded_rows.sort();
        self.written_names.sort();
        self.written_names.dedup();
        self.unrecognised.sort();
        self.pending_moves.sort();
        self.refused.sort();
        self.refused.dedup();
    }

    /// Is there anything worth printing? A clean fill on a store with no oddities reports nothing
    /// but its counts.
    ///
    /// ⚠ An ALIAS counts as something worth printing: a name the operator wrote has stopped being
    /// the live row for its credential, and a store that does that in silence is the defect §1 is
    /// about wearing a smaller hat.
    #[must_use]
    pub fn is_quiet(&self) -> bool {
        self.unrecognised.is_empty()
            && self.refused.is_empty()
            && self.pending_moves.is_empty()
            && self.aliases.is_empty()
    }

    /// Fold a second fill's findings into this one — a run that RESHAPES and then carries new file
    /// keys in the same transaction does both, and the operator wants ONE report.
    pub fn absorb(&mut self, other: RowReport) {
        self.accounts_created.extend(other.accounts_created);
        self.live_rows += other.live_rows;
        self.alias_rows += other.alias_rows;
        self.aliases.extend(other.aliases);
        self.superseded_rows.extend(other.superseded_rows);
        self.written_names.extend(other.written_names);
        self.notes_attached += other.notes_attached;
        self.unrecognised.extend(other.unrecognised);
        self.pending_moves.extend(other.pending_moves);
        self.refused.extend(other.refused);
        // ⚠ **A `SupersededKeyIsNotInTheStore` the OTHER half then wrote is not a finding**, and
        // leaving it in printed one on a run that did everything right. BOTH halves are handed the
        // same [`FileComments`], and a commented-out rollback line whose live key is arriving from
        // the FILE in this very run is seen twice: the reshape half runs first, over the OLD
        // table, where that key has no live row — so it refuses — and the fill half then inserts
        // the live row and rescues the same comment successfully. The refusal's own claim ("has no
        // live row in this store") is false by the time the transaction commits, and the evidence
        // is in this very report: the name is in `superseded_rows`. Dropped here rather than
        // suppressed at the source, because each half is individually right about the store it saw.
        let rescued: std::collections::BTreeSet<&str> =
            self.superseded_rows.iter().map(String::as_str).collect();
        self.refused.retain(|r| {
            !matches!(r, SchemaRefusal::SupersededKeyIsNotInTheStore { key }
                if rescued.contains(key.as_str()))
        });
        // `unattached_prose_lines` is a property of the FILE, not of a fill, so both halves saw the
        // same number and adding them would double it.
        self.sort();
    }
}

impl std::fmt::Display for RowReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} account row(s), {} live credential row(s)",
            self.accounts_created.len(),
            self.live_rows
        )?;
        if !self.superseded_rows.is_empty() {
            write!(
                f,
                ", {} superseded value(s) rescued from commented-out lines: {}",
                self.superseded_rows.len(),
                self.superseded_rows.join(", ")
            )?;
        }
        if !self.aliases.is_empty() {
            write!(
                f,
                "\n  {} name(s) are a SECOND SPELLING of a credential this store already holds — \
                 one account, one field, one value, two names. The live row is the canonical \
                 spelling's and the other is filed as its rollback copy; BOTH names still answer:",
                self.aliases.len()
            )?;
            for (alias, live) in &self.aliases {
                write!(f, "\n    - {alias} -> the live row is {live}'s")?;
            }
        }
        if self.notes_attached > 0 || self.unattached_prose_lines > 0 {
            write!(
                f,
                "\n  {} comment block(s) kept as notes; {} comment line(s) sat above no key and \
                 were attached to nothing",
                self.notes_attached, self.unattached_prose_lines
            )?;
        }
        if !self.unrecognised.is_empty() {
            write!(
                f,
                "\n  {} name(s) the classifier could not place — written VERBATIM as \
                 deployment-level rows, nothing dropped and nothing guessed: {}",
                self.unrecognised.len(),
                self.unrecognised.join(", ")
            )?;
        }
        if !self.pending_moves.is_empty() {
            write!(
                f,
                "\n  ⚠ {} row(s) are classified but NOT MOVED by this change — they keep their \
                 legacy name in `credential`, so every reader still finds them. The move waits on \
                 the map renderer (spec 6.2/12):",
                self.pending_moves.len()
            )?;
            for (name, mv) in &self.pending_moves {
                write!(f, "\n    - {name}: {mv}")?;
            }
        }
        if !self.refused.is_empty() {
            write!(
                f,
                "\n  ⚠ {} key(s) were REFUSED and nothing was written for them:",
                self.refused.len()
            )?;
            for r in &self.refused {
                write!(f, "\n    - {r}")?;
            }
        }
        Ok(())
    }
}

/// The `superseded_at` marker an ALIAS row carries.
///
/// Same shape and same reasoning as §4.2's `'superseded-before-schema-2'`: the column records THAT
/// a row is not the live one, and the reason rather than a date — nothing in this store knows when
/// the operator added the second spelling, and §4.3's rule forbids parsing prose for a fact code
/// uses. It is a distinct string from the rollback marker so the two can be told apart by eye in a
/// `SELECT`, which is the only way anybody will ever look at them.
const ALIAS_MARK: &str = "alias-of-the-canonical-tier-spelling";

/// The LIVE row at `(account_id, field)`, as `(id, name, value)`, or `None`.
///
/// `credential_one_live_value` admits at most one, so this is a lookup and not a scan.
fn live_row_at(
    tx: &Transaction<'_>,
    account_id: i64,
    field: &str,
) -> rusqlite::Result<Option<(i64, String, String)>> {
    let mut stmt = tx.prepare(
        "SELECT id, name, value FROM credential \
         WHERE account_id = ?1 AND field = ?2 AND superseded_at IS NULL",
    )?;
    let mut rows = stmt.query((account_id, field))?;
    match rows.next()? {
        Some(r) => Ok(Some((r.get(0)?, r.get(1)?, r.get(2)?))),
        None => Ok(None),
    }
}

/// **Does this NAME spell the tier its classification resolved to?**
///
/// The tiebreak when two names collide at one `(account, field)` with the same value, and the one
/// signal available here that is not row order. The classifier NORMALIZES a tier
/// (`vike_model::account_keys::AccountRef::tier`: the legacy `MAINNET` resolves to `LIVE`), so the
/// name that still carries the canonical token in its owner prefix is the canonical spelling and
/// the one that does not is the alias. `ASTER_LIVE_API_KEY` answers `true` here and
/// `ASTER_MAINNET_API_KEY` answers `false`, which is the whole of the decision.
///
/// The match is on `_{TIER}_` rather than on the bare token, so `DUKASCOPY_DEMO1_` does not read as
/// spelling `DEMO`. When NEITHER name spells the tier — every hand-mapped family, whose store token
/// is by definition not the canonical one (`ALPACA_SANDBOX_`) — the answer is `false` for both and
/// the row already written keeps the live value, i.e. sorted order decides. That is arbitrary and
/// is stated as such: it is reached only by a collision this store has never produced, and the
/// report NAMES both spellings either way.
fn spells_its_tier(name: &str, field: &str, tier: &str) -> bool {
    // An EMPTY tier would make the needle `__`, which a labelled name contains — so it is refused
    // outright rather than left to produce an answer from no evidence. Unreachable today (the
    // caller has a `Placement::Account`, whose tier is a non-empty `ACCOUNT_TIERS` member by the
    // time this runs), and cheap enough to state.
    if tier.is_empty() {
        return false;
    }
    let Some(prefix) = name.strip_suffix(field) else { return false };
    format!("_{prefix}").contains(&format!("_{}_", tier.to_ascii_uppercase()))
}

/// **Fill a schema-2 store's `account` and `credential` tables from a name→value map.**
///
/// The one derivation, used by BOTH a fresh create and [`reshape_into`], so a store born at 2 and a
/// store carried to 2 cannot be classified differently. It writes no `venue_setting` row and no
/// `venue_account_id` — see this module's doc for the sequencing rule that forbids both.
///
/// Rows already present under the same live NAME are left alone, which is what makes a second run a
/// no-op.
///
/// # Errors
/// The engine. Per-KEY refusals ride [`RowReport::refused`] and leave the run successful.
pub fn write_rows(
    tx: &Transaction<'_>,
    rows: &BTreeMap<String, String>,
    comments: &FileComments,
    classify: &dyn Fn(&str) -> Classification,
) -> rusqlite::Result<RowReport> {
    let mut report =
        RowReport { unattached_prose_lines: comments.unattached_prose_lines, ..Default::default() };
    let mut resolver = AccountResolver::load(tx)?;

    let existing: std::collections::BTreeSet<String> = {
        let mut stmt = tx.prepare("SELECT name FROM credential WHERE superseded_at IS NULL")?;
        let names = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = std::collections::BTreeSet::new();
        for n in names {
            out.insert(n?);
        }
        out
    };

    for (name, value) in rows {
        if existing.contains(name) {
            continue;
        }
        let class = classify(name);
        let note = comments.notes.get(name).map(String::as_str);
        let owner_prefix = class.owner_prefix(name);
        if owner_prefix.is_none() && class.needs_owner_prefix() {
            report.refused.push(SchemaRefusal::FieldIsNotASuffix {
                key: name.clone(),
                field: class.field.clone(),
            });
            continue;
        }
        let (account_id, venue) = match &class.placement {
            Placement::Account(key) => {
                if !ACCOUNT_TIERS.contains(&key.tier.as_str()) {
                    report.refused.push(SchemaRefusal::UnknownTier {
                        key: name.clone(),
                        tier: key.tier.clone(),
                    });
                    continue;
                }
                match resolver.resolve(tx, name, key, owner_prefix, note) {
                    Ok(id) => (Some(id), None),
                    Err(ResolveError::Sql(e)) => return Err(e),
                    Err(ResolveError::Refused(r)) => {
                        report.refused.push(r);
                        continue;
                    }
                }
            }
            Placement::Venue(v) => (None, Some(v.clone())),
            Placement::Infrastructure => (None, None),
        };
        // ⚠ **TWO NAMES, ONE `(account_id, field)`** — the one collision this store can actually
        // produce, and the reason this block exists rather than letting the engine answer. See
        // [`RowReport::aliases`] and [`SchemaRefusal::CollidingLiveValues`]; `spells_its_tier`
        // decides which of the two spellings keeps the live row.
        //
        // ⚠ Neither `INSERT` in this whole block names `venue_id`, and that is not an omission: both
        // fire only when `account_id` is `Some`, which happens only on the `Placement::Account` arm
        // above — and that arm always pairs `account_id = Some(id)` with `venue = None`. So `venue`
        // is provably `None` on every row either statement below can write; there is nothing for
        // `venue_id` to agree with. The two `credential` inserts further down, reached when
        // `account_id` is `None` (`Placement::Venue`/`Placement::Infrastructure`), are the ones that
        // can carry a non-NULL `venue` and are the ones that fill `venue_id`.
        if let Some(id) = account_id
            && let Some((other_id, other_name, other_value)) = live_row_at(tx, id, &class.field)?
        {
            if &other_value != value {
                report.refused.push(SchemaRefusal::CollidingLiveValues {
                    key: name.clone(),
                    other: other_name,
                    field: class.field.clone(),
                });
                continue;
            }
            let tier = match &class.placement {
                Placement::Account(key) => key.tier.as_str(),
                // Unreachable: `account_id` is `Some` only on the `Placement::Account` arm above.
                _ => "",
            };
            if spells_its_tier(name, &class.field, tier)
                && !spells_its_tier(&other_name, &class.field, tier)
            {
                // The NEWCOMER is the canonical spelling and the row already holding the live value
                // is the alias. Demote that one — an `UPDATE`, counted by neither counter — and let
                // this one land LIVE, which is an insert and counts as one.
                tx.execute(
                    "UPDATE credential SET superseded_at = ?2 WHERE id = ?1",
                    (other_id, ALIAS_MARK),
                )?;
                report.aliases.push((other_name, name.clone()));
                tx.execute(
                    "INSERT INTO credential \
                     (account_id, venue, field, value, name, secret, notes) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    (account_id, &venue, &class.field, value, name, i64::from(class.secret), note),
                )?;
                report.live_rows += 1;
            } else {
                // The ordinary direction: the row already written keeps the live value and THIS
                // name is filed as its rollback copy, so both names survive and `read_table`
                // answers for either.
                tx.execute(
                    "INSERT INTO credential \
                     (account_id, venue, field, value, name, secret, notes, superseded_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    (
                        account_id,
                        &venue,
                        &class.field,
                        value,
                        name,
                        i64::from(class.secret),
                        note,
                        ALIAS_MARK,
                    ),
                )?;
                report.aliases.push((name.clone(), other_name));
                report.alias_rows += 1;
            }
            report.written_names.push(name.clone());
            if note.is_some() {
                report.notes_attached += 1;
            }
            if let Some(mv) = class.pending_move {
                report.pending_moves.push((name.clone(), mv));
            }
            continue;
        }
        tx.execute(
            "INSERT INTO credential (account_id, venue, venue_id, field, value, name, secret, \
             notes) \
             VALUES (?1, ?2, (SELECT id FROM venue WHERE name = ?2), ?3, ?4, ?5, ?6, ?7)",
            (account_id, &venue, &class.field, value, name, i64::from(class.secret), note),
        )?;
        report.live_rows += 1;
        report.written_names.push(name.clone());
        if note.is_some() {
            report.notes_attached += 1;
        }
        if !class.recognised {
            report.unrecognised.push(name.clone());
        }
        if let Some(mv) = class.pending_move {
            report.pending_moves.push((name.clone(), mv));
        }
    }

    // §4.2 — the commented-out rollback copies, AFTER the live rows so the "is there a live row"
    // test below sees this run's own inserts.
    for (name, value) in &comments.superseded {
        let live: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM credential WHERE name = ?1 AND superseded_at IS NULL)",
            (name,),
            |r| r.get(0),
        )?;
        if !live {
            // ⚠ A commented assignment whose key has no live row is not a SUPERSEDED value — it is
            // a disabled key, and writing it would introduce a credential the store does not
            // otherwise hold, out of a line the one parser skips. Reported, not written.
            report.refused.push(SchemaRefusal::SupersededKeyIsNotInTheStore { key: name.clone() });
            continue;
        }
        let already: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM credential WHERE name = ?1 AND superseded_at IS NOT NULL)",
            (name,),
            |r| r.get(0),
        )?;
        if already {
            continue;
        }
        let class = classify(name);
        let owner_prefix = class.owner_prefix(name);
        let (account_id, venue) = match &class.placement {
            Placement::Account(key) => match resolver.resolve(tx, name, key, owner_prefix, None) {
                Ok(id) => (Some(id), None),
                Err(ResolveError::Sql(e)) => return Err(e),
                Err(ResolveError::Refused(r)) => {
                    report.refused.push(r);
                    continue;
                }
            },
            Placement::Venue(v) => (None, Some(v.clone())),
            Placement::Infrastructure => (None, None),
        };
        // `superseded_at` records that the value WAS replaced, not when — the file's comment is the
        // only evidence of a date and §4.3's rule forbids parsing prose for a fact code uses. The
        // marker is the migration that rescued it.
        tx.execute(
            "INSERT INTO credential \
             (account_id, venue, venue_id, field, value, name, secret, superseded_at) \
             VALUES (?1, ?2, (SELECT id FROM venue WHERE name = ?2), ?3, ?4, ?5, ?6, \
             'superseded-before-schema-2')",
            (account_id, &venue, &class.field, value, name, i64::from(class.secret)),
        )?;
        report.superseded_rows.push(name.clone());
        report.written_names.push(name.clone());
    }

    report.accounts_created = std::mem::take(&mut resolver.created);
    report.sort();
    Ok(report)
}

// ---------------------------------------------------------------------------------------------
// The reshape
// ---------------------------------------------------------------------------------------------

/// **Carry a schema-1 `credential` table into schema 2, IN PLACE, inside the caller's transaction.**
///
/// # ⚠ Why this is one transaction and not a sequence of steps
///
/// MEASURED in `db_tests::the_schema_stamp_and_the_ddl_are_both_transactional`: `PRAGMA
/// user_version` and DDL are BOTH rolled back with the transaction that set them. So the whole
/// reshape — the new table, the copy, the drop, the rename and the stamp — commits or does not, and
/// there is no state in between for a reader to find. That matters more here than it did for schema
/// 1's create, and in the opposite direction: schema 2's `credential` still has `name` and `value`
/// columns, so `SELECT name, value FROM credential` — the exact statement a schema-1 binary runs —
/// **is still valid SQL against the schema-2 shape**. A half-applied reshape stamped 1 over
/// schema-2 tables would therefore be ACCEPTED by an older binary and answered from silently.
/// Atomicity is what removes that state rather than documents it.
///
/// It follows that a reshape which fails leaves a working schema-1 store, re-runnable, with the
/// credential files beside it untouched — the same disposition `crate::db::migrate` takes about a
/// database it could not finish creating.
///
/// # The rebuild is a COPY, because `ALTER TABLE` cannot do it
///
/// Schema 1's `credential` is keyed `name TEXT PRIMARY KEY`; schema 2's is keyed on a surrogate
/// `id` with `name` merely unique among LIVE rows (§4.1 — a superseded row carries the same name as
/// the value that replaced it, which is what makes it a rollback copy). SQLite's `ALTER TABLE` can
/// add a column but cannot add or drop a PRIMARY KEY, so the old table is RENAMED aside, the new
/// one created from [`DDL`], the rows classified across, and the old one dropped.
///
/// ⚠ **No `VACUUM` afterwards.** It writes a full temp copy of the database — i.e. plaintext venue
/// credentials — into `SQLITE_TMPDIR` under the umask, which is the exact hazard `crate::db`'s
/// modes section exists to prevent and which its `preview` already refused once as an
/// implementation shortcut. The freed pages are reused by the next write; that is cheaper than a
/// second plaintext copy.
///
/// # Errors
/// The engine. Per-KEY refusals ride [`RowReport::refused`].
pub fn reshape_into(
    tx: &Transaction<'_>,
    comments: &FileComments,
    classify: &dyn Fn(&str) -> Classification,
) -> rusqlite::Result<RowReport> {
    let rows: BTreeMap<String, String> = {
        let mut stmt = tx.prepare("SELECT name, value FROM credential ORDER BY name")?;
        let it = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = BTreeMap::new();
        for row in it {
            let (n, v) = row?;
            out.insert(n, v);
        }
        out
    };

    tx.execute_batch(&format!("ALTER TABLE credential RENAME TO {RESHAPE_SCRATCH};"))?;
    tx.execute_batch(DDL)?;
    let report = write_rows(tx, &rows, comments, classify)?;

    // ⚠ **EVERY ROW MUST HAVE CARRIED, and a shortfall fails the RUN rather than one key.**
    //
    // This is the one place a per-key refusal is not survivable, and the difference from
    // `crate::db::migrate`'s is the SOURCE. There, a refused key is still in `secrets.env`, the run
    // carries its neighbours, and the operator re-runs after fixing one line. Here the source is
    // the table about to be DROPPED: a refusal that merely skipped a row would commit a schema-2
    // store missing a credential that exists nowhere else in the database, stamped at the current
    // version so every reader accepts it. A venue would silently drop to paper with nothing to say
    // why.
    //
    // The guard is a COUNT rather than an inspection of `refused`, deliberately: it catches any
    // future path that drops a row for a reason nobody has thought of yet, not just the three
    // refusals that exist today. The refused NAMES ride the message, because a count alone is not
    // something an operator can act on. Returning `Err` here rolls the whole transaction back — the
    // rename, the new tables and the stamp with it — so what is left on disk is the working
    // schema-1 store this call started from.
    //
    // ⚠ **ALIAS rows are added to the count**, and leaving them out turned the one collision this
    // store can produce into a whole-run failure. A `{VENUE}_MAINNET_*` key beside its
    // `{VENUE}_LIVE_*` twin is CARRIED — it is written, both names answer, and the report says so
    // — but it is written `superseded_at IS NOT NULL`, so it is not a LIVE row and `live_rows`
    // does not count it. The guard is about rows that VANISHED, and an alias did not.
    if report.live_rows + report.alias_rows != rows.len() {
        // ⚠ **The whole REFUSAL, not just its key.** This listed `SchemaRefusal::key` alone, which
        // is enough for the three refusals whose repair is obvious from the name and is NOT enough
        // for `CollidingLiveValues`, whose whole content is that this key collides with ANOTHER
        // one: an operator told only *the rows it could not carry: ASTER_MAINNET_API_KEY* has to
        // guess which other line in their file it disagrees with. Every variant's `Display` names a
        // key and never a value, so printing the refusal costs nothing the key did not.
        let missing: Vec<String> = report.refused.iter().map(ToString::to_string).collect();
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some(format!(
                "the schema upgrade read {} credential row(s) and could classify only {} — \
                 NOTHING WAS WRITTEN and this store is still at its old schema, which still \
                 reads. The rows it could not carry: {}. Every one of them exists only in this \
                 database, so dropping the old table would have destroyed them.",
                rows.len(),
                report.live_rows + report.alias_rows,
                if missing.is_empty() {
                    "(none named)".to_string()
                } else {
                    format!("\n    - {}", missing.join("\n    - "))
                },
            )),
        ));
    }

    tx.execute_batch(&format!("DROP TABLE {RESHAPE_SCRATCH};"))?;
    // The FK columns schema 2 introduces are checked before the caller commits, so a reshape that
    // produced a dangling `account_id` is an error rather than a store nobody notices is broken.
    // `PRAGMA foreign_keys` only enforces NEW statements; this asks about the whole database.
    let violations: i64 =
        tx.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get(0))?;
    if violations > 0 {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some(format!(
                "the reshaped credential store has {violations} dangling account reference(s); \
                 nothing was committed"
            )),
        ));
    }
    Ok(report)
}

// ---------------------------------------------------------------------------------------------
// The vocabulary's own properties
// ---------------------------------------------------------------------------------------------

/// **§4.4's rename, held to its own terms** — the two maps, and the one fact this crate spells
/// twice. The MIGRATION's proof is `crates/vike-secrets/tests/paper_tier.rs`, which drives a real
/// pre-rename store through a public writer; these are the pure halves.
#[cfg(test)]
mod schema_tests {
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
            assert!(
                armed.iter().any(|t| t == table),
                "`{table}` is armed in `DDL` — got {armed:?}"
            );
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
                "`{table}.{column}` must carry the measurement that proved it dead: a row here is \
                 a licence to destroy data on a live box"
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
            assert_eq!(
                account_tier_named(word),
                Some(PAPER_TIER),
                "{word:?} must name the paper tier"
            );
        }
        assert_eq!(account_tier_named("demo"), Some("demo"));
        assert_eq!(account_tier_named("live"), Some("live"));
        // …and a word that is not a tier answers `None` rather than being lowercased into one,
        // which is what lets the venue-settings grammar tell a tier segment from a FIELD.
        assert_eq!(account_tier_named("backend"), None);
        assert_eq!(account_tier_named("testnet"), None, "aster's own token is NOT a tier here");
    }
}
