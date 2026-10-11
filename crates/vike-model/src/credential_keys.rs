//! `credential_keys` — every credential/attribution key name this workspace can ever look up, as
//! DATA rather than as a format string.
//!
//! # The blind spot this closes
//!
//! `vike_bridge_core::credentials::load_credentials_from` builds each key with a `format!` over a
//! `{VENUE}_{TIER}` prefix and then asks a caller-supplied map for it; `attribution_code_from` does
//! the same with `{VENUE}_BROKER_CODE` / `{VENUE}_BUILDER_CODE`. The settings registry's scanner
//! (`crates/vike-model/src/scan.rs`) resolves string LITERALS and `const`s, so a key that never
//! appears as a literal anywhere had no `vike_ops::settings::SETTINGS` row **and no sighting** —
//! undetectably. `OKX_LIVE_API_SECRET` was read on every credential probe and was in neither place,
//! while its `OKX_DEMO_API_SECRET` sibling had a row only because a test fixture happened to spell
//! it; `BYBIT_BROKER_CODE` and three siblings had no fixture and so had no row at all. That is not
//! a gap `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `DYNAMIC_ALLOWLIST` can cover: it
//! allowlists a call site the scanner FOUND and could not resolve, and a computed map `get` is
//! never recognised as a candidate site to begin with.
//!
//! # The shape of the fix — the roster precedent
//!
//! One const table, plus completeness tests that iterate it. [`crate::venues::VENUES`] is the
//! exemplar every per-venue capability table already follows, and this module is the same move for
//! the key NAMES: [`CREDENTIAL_SUFFIXES`], [`ATTRIBUTION_SUFFIXES`] and [`CREDENTIAL_TIERS`] are
//! the only place those spellings exist, and [`credential_keys`] / [`attribution_keys`] fold them
//! over the roster into the whole enumerable grid.
//!
//! Three gates hold the two sides together, each comparing evidence of a DIFFERENT provenance:
//!
//! - `crates/vike-bridge-core/src/credentials.rs`'s
//!   `the_loader_reads_exactly_the_enumerated_credential_grid` folds the REAL loader's own
//!   `names_for_prefix` over the roster and every [`crate::venues::VENUES`] tier, and asserts the
//!   resulting name set IS [`credential_keys`] — so the enumeration cannot drift from the read.
//! - its `the_attribution_grid_is_exactly_what_the_reader_looks_up` twin drives the real
//!   `attribution_code_from` with a map holding one enumerated key at a time, and asserts BOTH
//!   ways: a mechanised venue's keys are enumerated and read, an unmechanised venue's are neither.
//! - `crates/vike-ops/tests/settings_secrets/settings_registry/walk_and_grid.rs`'s `every_generated_key_is_declared` demands a
//!   registry row for every key this module can produce.
//!
//! So adding a venue to [`crate::venues::VENUES`], a tier here, or a suffix here reddens the
//! registry until the rows exist — which is exactly what the roster contract promises for every
//! other per-venue table.
//!
//! # Key NAMES only
//!
//! Nothing here reads, holds, forwards or logs a credential VALUE: every function returns a
//! `String` that is a variable NAME, built from the caller's venue id and this module's own
//! constants. A credential never enters this file.
//!
//! # What the grid deliberately does NOT cover
//!
//! The BESPOKE per-venue shapes — `FXCM_{TIER}_USER`/`_PASSWORD`, `DUKASCOPY_DEMO1_LOGIN`,
//! `OANDA_{TIER}_ACCOUNT_ID`, `IG_{TIER}_IDENTIFIER`, `ASTER_{TIER}_PRIVATE_KEY`,
//! `HYPERLIQUID_{TIER}_PRIVATE_KEY`, the `POLY_*` L2 trio — are read by each bridge's own
//! `config.rs` loader with LITERAL keys, so the scanner has always seen them and they have always
//! had rows. This table is only for the family that is COMPUTED, which is the only family the
//! scanner is structurally blind to.
//!
//! ⚠ **The grid is an OVER-approximation over the roster, on purpose.**
//! `load_credentials_from` is generic over the venue string, and WHICH venues reach it is a `match`
//! arm rather than data: `vike_connections::status`'s `venue_env_configured` routes the venues whose
//! loaders read a bespoke key shape to those loaders, and everything else to the generic one. ⚠ That
//! set is deliberately not counted here — it was written as "six venues" and was wrong within the
//! month, when the four venues whose status had been read from a grid they never used grew arms of
//! their own. The `match` in `venue_env_configured` is the authority. So a roster venue's keys are
//! declared even where nothing asks for them today — the same rule the capability tables follow,
//! where a roster venue's row is always NAMED even when its value equals the fallback. The
//! alternative is a hand-kept "these venues use the generic loader" list, which is precisely the
//! copy that rots.

use crate::venues::VENUES;
use crate::venues::attribution::attribution_for;

/// The API-key suffix appended to a `{VENUE}_{TIER}` prefix.
pub const API_KEY_SUFFIX: &str = "_API_KEY";
/// The API-secret suffix appended to a `{VENUE}_{TIER}` prefix.
pub const API_SECRET_SUFFIX: &str = "_API_SECRET";
/// The API-passphrase suffix appended to a `{VENUE}_{TIER}` prefix. Required only where
/// `vike_bridge_core::venue_passphrase::venue_passphrase` says so, but LOOKED UP for every venue —
/// which is what puts it in the grid.
pub const API_PASSPHRASE_SUFFIX: &str = "_API_PASSPHRASE";

/// The three suffixes one `{VENUE}_{TIER}` prefix yields, in the order
/// `vike_bridge_core::credentials`' `names_for_prefix` returns them.
pub const CREDENTIAL_SUFFIXES: [&str; 3] =
    [API_KEY_SUFFIX, API_SECRET_SUFFIX, API_PASSPHRASE_SUFFIX];

/// The broker-code attribution suffix — tried FIRST by `attribution_code_from`.
pub const BROKER_CODE_SUFFIX: &str = "_BROKER_CODE";
/// The builder-code attribution suffix — the fallback `attribution_code_from` tries second.
pub const BUILDER_CODE_SUFFIX: &str = "_BUILDER_CODE";

/// Both attribution suffixes, in the order `attribution_code_from` tries them.
pub const ATTRIBUTION_SUFFIXES: [&str; 2] = [BROKER_CODE_SUFFIX, BUILDER_CODE_SUFFIX];

/// The environment tiers, spelled exactly as `vike_bridge_core::credentials::Environment::as_str`
/// spells them. Pinned equal to that enum by
/// `crates/vike-bridge-core/src/credentials/generated_key_grid_tests.rs`'s `the_environment_tiers_are_the_shared_table` —
/// neither crate can be the sole authority here (this one cannot see the enum, and the enum's crate
/// must not re-spell the grid), so the two are held equal by a gate.
///
/// ⚠ **There is no other tier spelling.** The pre-rename `MAINNET` token was deleted on the owner's
/// ruling of 2026-10-09 (venues take `DEMO` or `LIVE` credentials): a `{VENUE}_MAINNET_*` name is
/// an unknown name like any other, never read, never enumerated and never folded onto `LIVE`.
pub const CREDENTIAL_TIERS: [&str; 3] = ["SIM", "DEMO", "LIVE"];

/// **The PLATFORM keys — names this store holds that belong to no venue at all.**
///
/// The two `vike-tradehub` node keys: the observe (read) HMAC key and the control (write) one, in
/// the order a reader wants them. They live in the node-key store (the settings database's
/// `node_key` table) beside the venue credentials and are read from it by
/// a completely different loader
/// (`vike_tradehub_client::auth`'s `from_vars`, which the daemon calls through
/// `start_observe_server`), so the grid above has never covered them and must not start.
///
/// # What this table is FOR, and the three unions it deliberately is not part of
///
/// It exists so a WRITER can validate the names it is about to write against a fixed enumeration —
/// `crates/vike-cli/src/cmd/node/setup.rs` mints both keys and asks this table what to call them,
/// which is the answer `crates/vike-ops/tests/settings_secrets/credential_writer_gate/gates.rs`'s third `GROWTH_GUIDANCE`
/// question demands of every credential writer. It is a NAME table and nothing else: no value, no
/// tier, no venue, and no read.
///
/// - **NOT unioned into [`credential_keys`].** That function is asserted by
///   `crates/vike-bridge-core/src/credentials.rs`'s
///   `the_loader_reads_exactly_the_enumerated_credential_grid` to BE what `load_credentials_from`
///   reads, folded over the roster. These names are read by a different loader entirely, so
///   unioning them would make that gate assert something false.
/// - **NOT unioned into [`lookup_keys`], and therefore not into [`key_owner`].** `key_owner` is
///   `vike-cli secrets set`'s membership test, and that verb still REFUSES both names — the measured
///   pain was never "I could not type my key", it was "I had to invent one and nothing told me how".
///   A verb that accepts a hand-typed 256-bit HMAC key preserves the invention step, preserves the
///   copy-paste step, and adds a way to paste a truncated key that fails as an opaque auth denial.
///   Generating is the fix; accepting is not. `credential_keys.rs`'s own
///   `key_owner_classifies_exactly_the_lookup_grid` still lists the control key among its
///   `outsiders`, and that assertion is deliberately unchanged.
/// - **NOT unioned into [`starter_keys`].** That function is per-venue and exists to be shown to a
///   human writing their first venue store; there is no venue to show these under, and a store
///   template offering a key nobody should type by hand is the opposite of the point.
///
/// # ⚠ These spellings are a DUPLICATION, and the duplication is paid for
///
/// `vike_tradehub_client::auth`'s `OBSERVE_KEY_ENV` / `CONTROL_KEY_ENV` are the REFERENCE spelling —
/// that crate's doc carries the table of every copy and warns that a copy without an equality
/// assertion re-opens the gap it exists to close. This is such a copy, and its assertion is
/// `crates/vike-cli/tests/node_cli.rs`'s `the_platform_key_table_is_the_servers_own_spelling`,
/// which lives there because `vike-cli` is the lowest crate that can see BOTH this table (layer 10)
/// and the tradehub client (layer 50) — this crate cannot see that one, and must not.
///
/// ⚠ **`concat!` rather than whole literals, and it is load-bearing.**
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s literal harvest reads any string literal with
/// env-var shape and a known prefix as evidence that the containing crate READS that variable, and
/// then demands a `vike_ops::settings::SETTINGS` row for it. `vike-model` reads neither of these
/// names — it only names them — so a whole spelling here would make the registry assert something
/// false about this crate. Splitting the prefix off leaves two fragments neither of which is
/// env-shaped (one has no known prefix, the other does not begin with an uppercase letter) while the
/// compiled constant is byte-identical. It is the same move
/// `key_owner_classifies_exactly_the_lookup_grid` already makes with `format!` for its fixture.
/// ⚠ **FOUR names, not two, and the two that arrived late are the interesting half.** This table
/// held the TRADEHUB pair alone until 2026-09-08, while `vike-datahub` had grown an identical pair
/// of its own — same shape, same scopes, same HMAC handshake. Two consequences, both measured:
/// `vike-cli secrets set VIKE_DATAHUB_OBSERVE_KEY` fell through to the generic "edit it in by hand"
/// refusal instead of naming a command, and NOTHING in this tree could mint that pair, so the
/// datahub deployed on 2026-09-08 had its keys generated with `openssl` at a shell.
///
/// ORDER IS LOAD-BEARING and the tradehub pair stays first:
/// `crates/vike-cli/tests/node_cli.rs`'s `the_platform_key_table_is_the_servers_own_spelling`
/// asserts `[0]`/`[1]` against `vike_tradehub_client::auth`'s constants BY INDEX. The datahub pair
/// is APPENDED, and pays the same equality assertion in that file against
/// `vike_node_proto::auth`'s own spellings — the duplication rule below applies to it
/// identically.
/// ⚠ **FIVE now, and the fifth is NOT half of a pair.** `VIKE_TRADEHUB_ADMIN_KEY` joined on
/// 2026-09-20, for `docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md`'s
/// second barrier — the `Admin` scope, whose whole point is that *the key every desktop carries to
/// place orders is NOT the key that writes key material*. It is APPENDED at `[4]`, after the
/// datahub pair, because the index assertions above pin `[0..=3]`.
///
/// ⚠ **It is in THIS table and deliberately NOT in [`is_tradehub_node_key`]**, and that split is the
/// reason this doc grew rather than the entry being a one-liner. This table answers *may a node-key
/// writer write this name* — yes, that is how `vike-cli backend admin-key` mints it, and how
/// `secrets set` knows to route an operator to that command rather than to a text editor. That
/// predicate answers a DIFFERENT question — *does a file holding this name decide where the tradehub
/// PAIR is read from* — and for a third key used on its own the answer must be no. Its own doc
/// carries what happened the last time those two questions were answered by one table.
pub const PLATFORM_KEYS: [&str; 5] = [
    concat!("VIKE", "_TRADEHUB_OBSERVE_KEY"),
    concat!("VIKE", "_TRADEHUB_CONTROL_KEY"),
    concat!("VIKE", "_DATAHUB_OBSERVE_KEY"),
    concat!("VIKE", "_DATAHUB_CONTROL_KEY"),
    concat!("VIKE", "_TRADEHUB_ADMIN_KEY"),
];

/// Is `key` one of the [`PLATFORM_KEYS`]? The membership half of that table, so a caller asks a
/// question rather than reaching into an array.
///
/// ⚠ **This is NOT a second [`key_owner`], and must never be folded into one.** `key_owner` answers
/// *which venue and tier owns this name*, and its totality over [`lookup_keys`] — in BOTH directions
/// — is what `vike-cli secrets set`'s refusal rests on. This answers a disjoint question about a
/// disjoint name set, and the two tables' emptiness of intersection is asserted by
/// `platform_keys_are_outside_the_venue_grid` below.
#[must_use]
pub fn is_platform_key(key: &str) -> bool {
    PLATFORM_KEYS.contains(&key)
}

/// WHICH service's node keys `key` belongs to, as that service's binary name.
///
/// ⚠ **A caller that routes an operator somewhere needs this, and [`is_platform_key`] cannot give
/// it.** `vike-cli secrets set`'s refusal names the COMMAND that owns the key it is refusing, and
/// while there was one pair that command was a constant. With two services there are two commands,
/// and a refusal that named the wrong one would be the failure class that arm was built to end —
/// correct about the refusal, wrong about the route.
///
/// Returns the BINARY name rather than a bespoke enum on purpose: every caller is composing a
/// sentence for a human or picking a verb prefix, both of which want the name the operator already
/// knows, and an enum here would be a second vocabulary for a fact the string already carries.
/// `None` for anything outside the table, so this is safe to call before [`is_platform_key`].
#[must_use]
pub fn platform_key_service(key: &str) -> Option<&'static str> {
    match key {
        // ⚠ `[4]` — the ADMIN key — is tradehub's too, and belongs here for the reason this function
        // exists: a refusal has to name the COMMAND that owns the key it is refusing. It is
        // deliberately absent from [`is_tradehub_node_key`], which asks a different question; see
        // that predicate.
        k if k == PLATFORM_KEYS[0] || k == PLATFORM_KEYS[1] || k == PLATFORM_KEYS[4] => {
            Some(TRADEHUB_SERVICE)
        }
        k if k == PLATFORM_KEYS[2] || k == PLATFORM_KEYS[3] => Some(DATAHUB_SERVICE),
        _ => None,
    }
}

/// The two service names [`platform_key_service`] answers with, stated once so a caller asking
/// *"is this MY pair"* compares against the same spelling the classifier produced rather than a
/// fourth hand copy of a binary's name.
pub const TRADEHUB_SERVICE: &str = "vike-tradehub";
/// The datahub half of [`TRADEHUB_SERVICE`]'s pairing.
pub const DATAHUB_SERVICE: &str = "vike-datahub";

/// Is `key` the **tradehub** service's node-key pair — [`platform_key_service`] narrowed to one
/// family, in the shape `vike_secrets::resolve_node_keys` takes as its predicate?
///
/// `resolve_node_keys` hands back only the names its predicate admits, so the predicate IS the
/// scope its caller holds. The CLI and the desktop resolve the node store with THIS one and need the
/// observe/control pair only: a process that authenticates to ONE service never holds the other's
/// keys ([`is_platform_key`] would materialise the datahub pair as well — the blast-radius
/// argument), and the desktop never carries the key that writes key material (decision 0065). The
/// tradehub DAEMON is the one reader that also needs the admin key, and it has its own predicate:
/// [`is_tradehub_daemon_key`].
///
/// ⚠ **IT IS THE PAIR, NOT THE FAMILY — and since 2026-09-20 those differ.** This was
/// `platform_key_service(key) == Some(TRADEHUB_SERVICE)` while tradehub owned exactly two names.
/// `VIKE_TRADEHUB_ADMIN_KEY` is a third tradehub name, and 0065 gives it a separate scope precisely
/// so the key that trades is not the key that writes key material: folding it in here would hand
/// it to every reader of the pair.
#[must_use]
pub fn is_tradehub_node_key(key: &str) -> bool {
    key == PLATFORM_KEYS[0] || key == PLATFORM_KEYS[1]
}

/// Is `key` a name the tradehub **DAEMON** itself reads out of the node-key store — the observe and
/// control pair PLUS the admin key (`PLATFORM_KEYS[0]`, `[1]` and `[4]`, i.e. every platform key
/// [`platform_key_service`] files under [`TRADEHUB_SERVICE`])? The scope of
/// `vike_tradehub::node`'s `start_observe_server` and of nothing else.
///
/// ⚠ **Why the daemon needs the admin key and [`is_tradehub_node_key`] cannot give it.**
/// `vike_secrets::resolve_node_keys` returns only the names its predicate admits. The daemon arms
/// account administration (`config.tradehub_account_admin`, decision 0065's second barrier) only
/// when `VIKE_TRADEHUB_ADMIN_KEY` is in that map, so handing it the pair-only predicate leaves the
/// capability permanently absent: the daemon logs "no VIKE_TRADEHUB_ADMIN_KEY in this box's node-key
/// store" on a box that holds one. (Before `resolve_node_keys` filtered by family it returned the
/// whole table, which is why the daemon once armed with the pair-only predicate in its call.)
///
/// ⚠ **Why [`is_tradehub_node_key`] STAYS the pair.** The CLI (`vike-cli`'s node keyring) and the
/// desktop (the backend registry's node overlay) read the observe/control pair and never the admin
/// key; widening the shared predicate would hand the desktop the key that writes key material, the
/// one thing decision 0065's `Admin` scope exists to keep off it. So the wider scope is a SEPARATE
/// name, and `crates/vike-ops/tests/settings_secrets/node_key_store_gate.rs` lets exactly one production
/// file name it (the daemon's), so a CLI or GUI call passing it turns that gate red.
///
/// ⚠ **The datahub pair is NOT admitted.** This process authenticates to its own service; it never
/// holds the datahub's keys (the blast-radius argument of [`is_tradehub_node_key`]).
#[must_use]
pub fn is_tradehub_daemon_key(key: &str) -> bool {
    platform_key_service(key) == Some(TRADEHUB_SERVICE)
}

/// Is `key` the **datahub** service's node-key pair? [`is_tradehub_node_key`]'s twin, and its doc
/// carries the argument for both: a process that authenticates to the datahub never holds the
/// tradehub's keys.
#[must_use]
pub fn is_datahub_node_key(key: &str) -> bool {
    platform_key_service(key) == Some(DATAHUB_SERVICE)
}

/// **The names this workspace reads OUT OF THE CREDENTIAL STORE that are neither in the venue grid
/// ([`lookup_keys`]) nor node keys ([`PLATFORM_KEYS`]) nor a name the store's own classifier
/// recognises** — each beside the crate `vike_ops::settings` records reading it.
///
/// # What it is FOR
///
/// `vike-cli secrets set` may write a name outside the grid only when something reads it, and the
/// classifier-based rule it had (`crates/vike-cli/src/cmd/secrets/set.rs`'s
/// `settable_outside_the_grid`) cannot reach these: the classifier files them in its
/// `Infrastructure` CATCH-ALL beside every unrecognised string, and the registry alone cannot tell
/// them from a settings key read off the process environment (`VIKE_HIST_STORE` has rows of the same
/// shape). Before this table they were writable only once the store ALREADY held them (the ROTATION
/// rule), so a box with a settings database and no copy of, say, the pager token had no way to add
/// one: the credential FILE store a human used to edit was removed on 2026-10-07.
///
/// # Why a CLOSED, named table rather than a predicate
///
/// Every predicate over the NAME either admits a settings key or refuses one of these; whether a
/// read consults the credential map is a fact about the CALL SITE, which no name carries. So each
/// row was read off its call site (the map the reader is handed is the one
/// `vike_bridge_core::credentials::load_workspace_secrets_*` loads), and
/// `crates/vike-cli/src/cmd/secrets/tests/set_tests.rs`'s
/// `the_settable_list_and_the_registry_agree_both_ways` holds it to the registry in BOTH directions:
/// a row here must name a registry row of that `(name, krate)` (a stale name cannot stay settable),
/// and every registry map-lookup read outside the grid, the node keys, the classifier and this table
/// must be argued in that test's table of reads that are NOT the credential store (a new
/// credential-store read cannot arrive unsettable).
///
/// Sorted by name. ⚠ `concat!`-split for the reason [`PLATFORM_KEYS`] gives: a whole env-shaped
/// literal here would read to the settings registry's harvest as `vike-model` reading the variable.
pub const STORE_KEYS_OUTSIDE_THE_GRID: [(&str, &str); 19] = [
    // The Studio chat pane's Anthropic key: `vike_studio::ChatApiKeys::resolve` looks it up in the
    // credential map the desktop loads from the settings database. ⚠ `vike-agent-eval` also reads a
    // variable of this name, off the PROCESS environment — a different row, never the store.
    (concat!("ANTHROPIC", "_API_KEY"), "vike-studio"),
    // The aster mount's builder fee, read out of the mount's credential map.
    (concat!("ASTER", "_BUILDER_FEE_RATE"), "bridges/aster"),
    // Aster's DEMO-tier agent wallet, read out of the mount's credential map. ⚠ A VENUE credential
    // the classifier does not place: `TESTNET` is aster's spelling of its demo tier and no
    // `vike_secrets::venue_setting::HAND_MAPPED_ACCOUNTS` row maps it, so the ledger files these under
    // the deployment until one does — and the day it does, the registry gate named above reddens
    // (another rule owns them) and these three rows leave.
    (concat!("ASTER", "_TESTNET_PRIVATE_KEY"), "bridges/aster"),
    (concat!("ASTER", "_TESTNET_SIGNER"), "vike-connections"),
    (concat!("ASTER", "_TESTNET_USER"), "bridges/aster"),
    // The Studio chat pane's Cerebras key, the same credential-map lookup as the Anthropic one.
    (concat!("CEREBRAS", "_API_KEY"), "vike-studio"),
    // `databento_backfill`'s `api_key`: the scoped credential-store read first, the process
    // environment only as a fallback.
    (concat!("DATABENTO", "_API_KEY"), "vike-backfill"),
    // The hyperliquid mount's builder fee, beside the builder code it qualifies.
    (concat!("HYPERLIQUID", "_BUILDER_FEE_TENTHS_BP"), "bridges/hyperliquid"),
    // The dukascopy mount's JForex sidecar tools, resolved out of the mount's credential map.
    (concat!("JAVA", "_HOME"), "bridges/dukascopy"),
    (concat!("JFOREX", "_BRIDGE_JAR"), "bridges/dukascopy"),
    // `tardis_backfill`'s `api_key`: the scoped credential-store read, no environment fallback.
    (concat!("TARDIS", "_API_KEY"), "vike-backfill"),
    // The pager: `vike_alerting::delivery`'s targets, handed the daemon's credential map (and the
    // datahub's scoped store read).
    (concat!("VIKE", "_ALERT_TELEGRAM_CHAT_ID"), "vike-alerting"),
    (concat!("VIKE", "_ALERT_TELEGRAM_TOKEN"), "vike-alerting"),
    (concat!("VIKE", "_ALERT_WEBHOOK_URL"), "vike-alerting"),
    // The cohort/collector key — `vikedata_backfill`'s scoped credential-store read.
    (concat!("VIKE", "_API_KEY"), "vike-backfill"),
    // The archive and events-API backfills' key, the same scoped read.
    (concat!("VIKE", "_ARCHIVE_API_KEY"), "vike-backfill"),
    // The tradehub's Telegram control bot, configured from the daemon's credential map.
    (concat!("VIKE", "_TELEGRAM_ALLOWED_CHAT_IDS"), "vike-tradehub"),
    (concat!("VIKE", "_TELEGRAM_ALLOWED_USER_IDS"), "vike-tradehub"),
    (concat!("VIKE", "_TELEGRAM_BOT_TOKEN"), "vike-tradehub"),
];

/// Is `key` one of [`STORE_KEYS_OUTSIDE_THE_GRID`]? The membership half, so a writer asks a question
/// rather than reaching into the table.
#[must_use]
pub fn is_store_key_outside_the_grid(key: &str) -> bool {
    STORE_KEYS_OUTSIDE_THE_GRID.iter().any(|(name, _)| *name == key)
}

/// One credential key: `{VENUE}_{TIER}{SUFFIX}`, e.g. `BINANCE_DEMO_API_KEY`.
///
/// `venue` is a canonical lowercase roster id ([`crate::venues::VENUES`]); the uppercasing is the
/// loader's own (`load_credentials_from` builds its prefix the same way), so this and the read can
/// only agree.
#[must_use]
pub fn credential_key(venue: &str, tier: &str, suffix: &str) -> String {
    format!("{}_{tier}{suffix}", venue.to_uppercase())
}

/// One attribution key: `{VENUE}{SUFFIX}`, e.g. `OKX_BROKER_CODE`.
///
/// ⚠ `attribution_code_from` used to uppercase with `to_ascii_uppercase` here while the credential
/// loader used `to_uppercase`; both call this now. The difference is unreachable, not merely
/// unlikely — that reader returns `None` for a venue with no [`crate::venues::attribution`] mechanic
/// BEFORE it builds a key, and every mechanic arm is matched on a lowercase ASCII roster id, so no
/// string whose two uppercasings differ can reach this function through it. This file's
/// `the_two_uppercasings_agree_on_every_roster_venue` pins that for the roster.
#[must_use]
pub fn attribution_key(venue: &str, suffix: &str) -> String {
    format!("{}{suffix}", venue.to_uppercase())
}

/// **The attribution key a venue's MECHANIC implies** — `None` for a venue with no order-level
/// mechanic, which is the same narrowing [`attribution_keys`] applies.
///
/// A `SignedBuilder` venue takes [`BUILDER_CODE_SUFFIX`] and every other mechanic takes
/// [`BROKER_CODE_SUFFIX`], which is the split `crates/vike-bridge-core/CLAUDE.md`'s
/// attribution-codes bullet states venue by venue. Derived from [`crate::venues::attribution::attribution_for`] rather than listed, so a
/// new mechanised venue is classified by adding no row here.
///
/// # Why a caller wants THIS rather than [`attribution_key`]
///
/// `attribution_code_from` accepts EITHER spelling — it tries the broker name and falls back to the
/// builder one — so both names exist in the grid for every mechanised venue and neither is wrong to
/// read. A WRITER has to pick one, and picking it per venue is a per-venue table; picking it from
/// the mechanic is not. The Connections editor's credential form is the caller this exists for: it
/// offers exactly one attribution field, because two fields over one tag is a control where filling
/// the wrong one loses silently to the other.
///
/// ⚠ **It also exists so that caller does not have to CALL [`attribution_key`]**, and that is not a
/// stylistic preference. `crates/vike-ops/tests/settings_secrets/settings_registry/walk_and_grid.rs`'s `generated_key_sites` reads
/// any file calling one of the grid builders as *this crate READS the whole grid* and then demands
/// a `vike_ops::settings::SETTINGS` row for every one of its several hundred names. A form that
/// composes ONE attribution name reads none of them, so the composition belongs here — the table's
/// own module, which that gate excludes by construction — exactly as [`starter_keys`] and
/// [`key_owner`] do for the same reason.
#[must_use]
pub fn attribution_var_for(venue: &str) -> Option<String> {
    let mech = attribution_for(venue);
    if mech.is_none() {
        return None;
    }
    let suffix = match mech {
        crate::venues::attribution::AttributionMechanic::SignedBuilder { .. } => {
            BUILDER_CODE_SUFFIX
        }
        _ => BROKER_CODE_SUFFIX,
    };
    Some(attribution_key(venue, suffix))
}

/// The WHOLE credential grid: every roster venue × every tier × every suffix, sorted and
/// deduplicated.
///
/// Allocates, and is meant to: the callers are the registry gate and the loader's own equivalence
/// test, never a hot path. `load_credentials_from` builds the two or three names it needs and does
/// not walk this.
#[must_use]
pub fn credential_keys() -> Vec<String> {
    let mut out: Vec<String> = VENUES
        .iter()
        .flat_map(|venue| {
            CREDENTIAL_TIERS.iter().flat_map(move |tier| {
                CREDENTIAL_SUFFIXES.iter().map(move |sfx| credential_key(venue, tier, sfx))
            })
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Every attribution key that can be looked up, sorted and deduplicated.
///
/// Only venues with an order-level [`crate::venues::attribution::AttributionMechanic`] appear: a venue
/// classified `None` makes `attribution_code_from` return before any key is built, so declaring one
/// would claim a read that provably cannot happen. That is the one place this module is NARROWER
/// than the roster, and it is derived from the capability table rather than hand-listed.
#[must_use]
pub fn attribution_keys() -> Vec<String> {
    let mut out: Vec<String> = VENUES
        .iter()
        .filter(|venue| !attribution_for(venue).is_none())
        .flat_map(|venue| ATTRIBUTION_SUFFIXES.iter().map(move |sfx| attribution_key(venue, sfx)))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// [`credential_keys`] ∪ [`attribution_keys`] — the whole set of names a computed map `get` in this
/// workspace can ask for, sorted and deduplicated. The registry gate's input.
#[must_use]
pub fn lookup_keys() -> Vec<String> {
    let mut out = credential_keys();
    out.extend(attribution_keys());
    out.sort();
    out.dedup();
    out
}

/// **Which venue and tier a [`lookup_keys`] name belongs to** — the classification a WRITER needs
/// and a reader never did.
///
/// `Some((venue, Some(tier)))` for a credential key, `Some((venue, None))` for an attribution key
/// (a broker/builder code is per-venue and has no tier), and `None` for any name outside
/// [`lookup_keys`] — so this is also the membership test, answered once instead of by building the
/// whole grid and searching it.
///
/// ⚠ **It lives HERE for the reason [`starter_keys`] gives**, and the reason is a gate rather than
/// taste: `crates/vike-ops/tests/settings_secrets/settings_registry/walk_and_grid.rs`'s `generated_key_sites` reads a call to
/// [`credential_key`] as *this crate READS these variables* and would then demand the whole grid's
/// worth of `SETTINGS` rows for whichever crate composed the names. `vike-cli secrets set`
/// validates a key name and reads none of them, so the composition belongs in the table's own
/// module and the caller just asks.
#[must_use]
pub fn key_owner(key: &str) -> Option<(&'static str, Option<&'static str>)> {
    for venue in VENUES {
        for tier in CREDENTIAL_TIERS.iter() {
            for sfx in CREDENTIAL_SUFFIXES {
                if credential_key(venue, tier, sfx) == key {
                    return Some((venue, Some(tier)));
                }
            }
        }
        // Same narrowing `attribution_keys` applies: a venue with no order-level mechanic produces
        // no attribution key, so one spelled for it belongs to nobody.
        if !attribution_for(venue).is_none() {
            for sfx in ATTRIBUTION_SUFFIXES {
                if attribution_key(venue, sfx) == key {
                    return Some((venue, None));
                }
            }
        }
    }
    None
}
/// The keys a FIRST store should carry for ONE venue, in the order a human wants to read them:
/// every tier × every credential suffix, then the attribution keys this venue's mechanic can
/// actually produce.
///
/// ⚠ **This lives HERE rather than in the CLI that prints it, and that placement is load-bearing.**
/// `crates/vike-ops/tests/settings_secrets/settings_registry/walk_and_grid.rs`'s `generated_key_sites` treats any file CALLING
/// [`credential_key`] as a site whose crate must then declare the whole grid in `SETTINGS` — the
/// gate's meaning is *this crate READS these variables*. A command that merely prints key NAMES
/// reads none of them, so composing them at the call site would have made the registry assert
/// something false about `vike-cli`. This module is the table's own definition and is excluded from
/// that set by construction, so the composition belongs here and the caller just renders.
#[must_use]
pub fn starter_keys(venue: &str) -> Vec<String> {
    let mut out: Vec<String> = CREDENTIAL_TIERS
        .iter()
        .flat_map(|tier| {
            CREDENTIAL_SUFFIXES.iter().map(move |sfx| credential_key(venue, tier, sfx))
        })
        .collect();
    // Only venues with an order-level mechanic produce attribution keys; reuse that classification
    // rather than restating it, exactly as `attribution_keys` does.
    if !attribution_for(venue).is_none() {
        out.extend(ATTRIBUTION_SUFFIXES.iter().map(|sfx| attribution_key(venue, sfx)));
    }
    out
}

#[path = "credential_keys_tests.rs"]
#[cfg(test)]
mod credential_keys_tests;
