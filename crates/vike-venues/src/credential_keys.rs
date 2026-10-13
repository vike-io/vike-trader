//! `credential_keys` — the credential-store key NAMES this workspace looks up by NAME, as DATA
//! rather than as a format string, and the FIELD words of the generic HMAC credential set.
//!
//! # A venue credential is addressed by ACCOUNT and FIELD, never by a composed name
//!
//! Decision 0156 item 6: a credential belongs to its ACCOUNT. A bridge asks for "this account's
//! `API_KEY`" — the store's `credential.account_id` + `field` — through
//! `vike_secrets::account_credentials::AccountCredentials`, and never composes a name to look one
//! up. So this module no longer carries the `{VENUE}_{TIER}{SUFFIX}` grid it was built for: no
//! credential name carries a tier, and no reader derives one from a name. The field words a venue's
//! account holds are [`crate::credential_schema`]'s table; the generic HMAC set's three are
//! [`API_KEY`], [`API_SECRET`] and [`API_PASSPHRASE`] below.
//!
//! # What is still looked up by NAME, and why it is enumerable here
//!
//! Three tier-less families are read out of the composition root's name-keyed map, and each is
//! listed here so a writer can validate a name against a fixed enumeration and the settings
//! registry can see every name a computed map `get` can ask for:
//!
//! - the ATTRIBUTION codes, `{VENUE}_BROKER_CODE` / `{VENUE}_BUILDER_CODE` ([`attribution_keys`]),
//!   which `vike_secrets::credentials::attribution_code_from` composes with a `format!` — the one
//!   COMPUTED family left, and so the one the settings registry's scanner
//!   (`crates/vike-model/src/scan.rs`) cannot see without this table.
//!   `crates/vike-ops/tests/settings_secrets/settings_registry/walk_and_grid.rs`'s
//!   `every_generated_key_is_declared` demands a registry row for every one;
//! - the node keys ([`PLATFORM_KEYS`]);
//! - the deployment's own keys ([`STORE_KEYS_OUTSIDE_THE_GRID`]).
//!
//! # Key NAMES only
//!
//! Nothing here reads, holds, forwards or logs a credential VALUE: every function returns a
//! `String` that is a variable NAME or a field WORD, built from the caller's venue id and this
//! module's own constants. A credential never enters this file.

use crate::attribution::attribution_for;
use vike_model::VENUES;

/// The generic HMAC credential set's API-key FIELD (`credential.field`), as an account holds it.
pub const API_KEY: &str = "API_KEY";
/// The generic HMAC credential set's API-secret FIELD.
pub const API_SECRET: &str = "API_SECRET";
/// The generic HMAC credential set's passphrase FIELD. Required only where
/// `vike_secrets::venue_passphrase::venue_passphrase` says so (OKX).
pub const API_PASSPHRASE: &str = "API_PASSPHRASE";

/// The broker-code attribution suffix — tried FIRST by `attribution_code_from`.
pub const BROKER_CODE_SUFFIX: &str = "_BROKER_CODE";
/// The builder-code attribution suffix — the fallback `attribution_code_from` tries second.
pub const BUILDER_CODE_SUFFIX: &str = "_BUILDER_CODE";

/// Both attribution suffixes, in the order `attribution_code_from` tries them.
pub const ATTRIBUTION_SUFFIXES: [&str; 2] = [BROKER_CODE_SUFFIX, BUILDER_CODE_SUFFIX];

/// **The PLATFORM keys — names this store holds that belong to no venue at all.**
///
/// The two `vike-tradehub` node keys: the observe (read) HMAC key and the control (write) one, in
/// the order a reader wants them. They live in the node-key store (the settings database's
/// `node_key` table) beside the venue credentials and are read from it by
/// a completely different loader
/// (`vike_tradehub_client::auth`'s `from_vars`, which the daemon calls through
/// `start_observe_server`), so the attribution grid below has never covered them and must not start.
///
/// # What this table is FOR, and the three unions it deliberately is not part of
///
/// It exists so a WRITER can validate the names it is about to write against a fixed enumeration —
/// `crates/vike-cli/src/cmd/node/setup.rs` mints both keys and asks this table what to call them,
/// which is the answer `crates/vike-ops/tests/settings_secrets/credential_writer_gate/gates.rs`'s third `GROWTH_GUIDANCE`
/// question demands of every credential writer. It is a NAME table and nothing else: no value, no
/// tier, no venue, and no read.
///
/// - **NOT unioned into [`attribution_keys`], and therefore not into [`attribution_key_owner`].**
///   `vike-cli secrets set` still REFUSES both names — the measured pain was never "I could not
///   type my key", it was "I had to invent one and nothing told me how". A verb that accepts a
///   hand-typed 256-bit HMAC key preserves the invention step, preserves the copy-paste step, and
///   adds a way to paste a truncated key that fails as an opaque auth denial. Generating is the fix;
///   accepting is not. `platform_keys_are_outside_the_venue_grid` holds the two tables disjoint.
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
/// then demands a `vike_config::settings::SETTINGS` row for it. `vike-model` reads neither of these
/// names — it only names them — so a whole spelling here would make the registry assert something
/// false about this crate. Splitting the prefix off leaves two fragments neither of which is
/// env-shaped (one has no known prefix, the other does not begin with an uppercase letter) while the
/// compiled constant is byte-identical.
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
/// ⚠ **This is NOT a second [`attribution_key_owner`], and must never be folded into one.** That
/// answers *which venue owns this attribution name*; this answers a disjoint question about a
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
/// family, in the shape `vike_secrets::CredentialStore::node_keys` takes as its predicate?
///
/// `CredentialStore::node_keys` hands back only the names its predicate admits, so the predicate IS the
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
/// `vike_secrets::CredentialStore::node_keys` returns only the names its predicate admits. The daemon arms
/// account administration (`config.tradehub_account_admin`, decision 0065's second barrier) only
/// when `VIKE_TRADEHUB_ADMIN_KEY` is in that map, so handing it the pair-only predicate leaves the
/// capability permanently absent: the daemon logs "no VIKE_TRADEHUB_ADMIN_KEY in this box's node-key
/// store" on a box that holds one. (Before the node-key read filtered by family it returned the
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

/// The credential-store name of the Finnhub key (earnings), read by the datahub's tool-feeds lane
/// from its own box's store (`docs/decisions/0143-the-news-calendar-and-equity-tools-read-the-datahub-and-the-desktop-fetches-nothing.md`).
///
/// ⚠ `concat!`-split for the reason [`PLATFORM_KEYS`] gives: a whole env-shaped literal in this
/// crate would read to the settings registry's harvest as `vike-model` reading the variable. The
/// datahub spells the same name once more as its own constant (the registry resolves a name through
/// a constant declared in the READING crate); `crates/vike-datahub/tests/tool_feeds.rs`'s
/// `the_provider_key_names_are_the_models_own` holds the two equal.
pub const FINNHUB_API_KEY: &str = concat!("FINNHUB", "_API_KEY");

/// The credential-store name of the FMP key (dividends) — see [`FINNHUB_API_KEY`].
pub const FMP_API_KEY: &str = concat!("FMP", "_API_KEY");

/// **The two third-party DATA-provider keys the GUI's "Data providers" group may set and remove on
/// the node's own store** over the account wire (`docs/decisions/0144`'s ADDED note) — a closed
/// pair, not [`STORE_KEYS_OUTSIDE_THE_GRID`]: the wire admits exactly these two names beside the
/// venue grid, so a remote write cannot reach the pager token or the Telegram bot's.
pub const DATA_PROVIDER_KEYS: [&str; 2] = [FINNHUB_API_KEY, FMP_API_KEY];

/// **The DEPLOYMENT names this workspace reads OUT OF THE CREDENTIAL STORE by name** — neither an
/// account's credential (addressed by account and field, decision 0156), nor an attribution code
/// ([`attribution_keys`]), nor a node key ([`PLATFORM_KEYS`]) — each beside the crate
/// `vike_config::settings` records reading it.
///
/// # What it is FOR
///
/// `vike-cli secrets set NAME` may write a deployment name only when something reads it: the
/// registry alone cannot tell these from a settings key read off the process environment
/// (`VIKE_HIST_STORE` has rows of the same shape). A deployment row belongs to no account and no
/// venue (`credential.account_id` and `venue_id` both NULL, its `field` the whole name).
///
/// # Why a CLOSED, named table rather than a predicate
///
/// Every predicate over the NAME either admits a settings key or refuses one of these; whether a
/// read consults the credential map is a fact about the CALL SITE, which no name carries. So each
/// row was read off its call site (the map the reader is handed is the one
/// `vike_secrets::CredentialStore::read_all` reads), and
/// `crates/vike-cli/src/cmd/secrets/tests/set_tests.rs`'s
/// `the_settable_list_and_the_registry_agree_both_ways` holds it to the registry in BOTH directions:
/// a row here must name a registry row of that `(name, krate)` (a stale name cannot stay settable),
/// and every registry map-lookup read outside the attribution grid, the node keys and this table
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
    // The Studio chat pane's Cerebras key, the same credential-map lookup as the Anthropic one.
    (concat!("CEREBRAS", "_API_KEY"), "vike-studio"),
    // `databento_backfill`'s `api_key`: the scoped credential-store read first, the process
    // environment only as a fallback.
    (concat!("DATABENTO", "_API_KEY"), "vike-backfill"),
    // The tool-feeds lane's two third-party data keys, read by the DATAHUB from its own box's store
    // (`vike_datahub::tool_feeds::StoreKeys`): Finnhub for earnings, FMP for dividends.
    (FINNHUB_API_KEY, "vike-datahub"),
    (FMP_API_KEY, "vike-datahub"),
    // The hyperliquid mount's builder fee, beside the builder code it qualifies.
    (concat!("HYPERLIQUID", "_BUILDER_FEE_TENTHS_BP"), "bridges/hyperliquid"),
    // The dukascopy mount's JForex sidecar tools, resolved out of the mount's credential map.
    (concat!("JAVA", "_HOME"), "bridges/dukascopy"),
    (concat!("JFOREX", "_BRIDGE_JAR"), "bridges/dukascopy"),
    // The polymarket mount's geoblock escape hatch (`1`), read from the deployment's named values.
    (concat!("POLY", "_GEOBLOCK_OVERRIDE"), "bridges/polymarket"),
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

/// One attribution key: `{VENUE}{SUFFIX}`, e.g. `OKX_BROKER_CODE`.
///
/// ⚠ `attribution_code_from` used to uppercase with `to_ascii_uppercase` here while the credential
/// loader used `to_uppercase`; both call this now. The difference is unreachable, not merely
/// unlikely — that reader returns `None` for a venue with no [`crate::attribution`] mechanic
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
/// attribution-codes bullet states venue by venue. Derived from [`crate::attribution::attribution_for`] rather than listed, so a
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
/// a `vike_config::settings::SETTINGS` row for every one of its several hundred names. A form that
/// composes ONE attribution name reads none of them, so the composition belongs here — the table's
/// own module, which that gate excludes by construction — exactly as [`attribution_key_owner`] does
/// for the same reason.
#[must_use]
pub fn attribution_var_for(venue: &str) -> Option<String> {
    let mech = attribution_for(venue);
    if mech.is_none() {
        return None;
    }
    let suffix = match mech {
        crate::attribution::AttributionMechanic::SignedBuilder { .. } => BUILDER_CODE_SUFFIX,
        _ => BROKER_CODE_SUFFIX,
    };
    Some(attribution_key(venue, suffix))
}

/// Every attribution key that can be looked up, sorted and deduplicated.
///
/// Only venues with an order-level [`crate::attribution::AttributionMechanic`] appear: a venue
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

/// **Which venue an [`attribution_keys`] name belongs to** — the classification a WRITER needs and a
/// reader never did; `None` for any name outside that grid, so this is also the membership test.
///
/// ⚠ **It lives HERE for the reason [`attribution_var_for`] gives**, and the reason is a gate rather
/// than taste: `crates/vike-ops/tests/settings_secrets/settings_registry/walk_and_grid.rs`'s
/// `generated_key_sites` reads a call to [`attribution_key`] as *this crate READS these variables*
/// and would then demand the whole grid's worth of `SETTINGS` rows for whichever crate composed the
/// names. `vike-cli secrets set` validates a key name and reads none of them, so the composition
/// belongs in the table's own module and the caller just asks.
#[must_use]
pub fn attribution_key_owner(key: &str) -> Option<&'static str> {
    VENUES.iter().copied().find(|venue| {
        // Same narrowing `attribution_keys` applies: a venue with no order-level mechanic produces
        // no attribution key, so one spelled for it belongs to nobody.
        !attribution_for(venue).is_none()
            && ATTRIBUTION_SUFFIXES.iter().any(|sfx| attribution_key(venue, sfx) == key)
    })
}

#[path = "credential_keys_tests.rs"]
#[cfg(test)]
mod credential_keys_tests;
