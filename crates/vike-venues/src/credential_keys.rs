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
//! - the hub keys ([`PLATFORM_KEYS`]);
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
/// The hub keys. They live in the hub-key store (the settings database's `hub_key` table) beside the
/// venue credentials and are read from it by each DAEMON's own loader, so the attribution grid below
/// has never covered them and must not start.
///
/// ⚠ **ORDER IS LOAD-BEARING**: tests across the tree pin these names BY INDEX, so a new name is
/// APPENDED and a retired one keeps its slot.
///
/// - `[0]`..`[4]` are [`RETIRED_HUB_KEYS`], taken from that one list in its order (its doc says what
///   they were and why they stay).
/// - `[5]`/`[6]` are the GATEWAYHUB's pair (`docs/decisions/0164-one-daemon-owns-every-venue-connection-vike-gatewayhub.md`,
///   verdict 6): VERIFIED by the gatewayhub daemon and HELD, as a client, by the DATAHUB, the
///   gatewayhub's one price consumer. [`gatewayhub_key_pair`] is its named read.
/// - `[7]` `VIKE_GATEWAYHUB_TRADE_KEY` is the ORDER-scope key of the gatewayhub (`Scope::Trade`):
///   VERIFIED by the gatewayhub daemon and HELD, as a client, by the tradehub.
///   [`gatewayhub_trade_key`] is its named read.
/// - `[8]` `VIKE_GATEWAYHUB_ADMIN_KEY` is NAMED here and minted and served by NOTHING: the
///   gatewayhub's `Scope::Account` stays refused until a later wave, and `vike-cli gatewayhub setup`
///   does not mint it.
/// - `[9]` `VIKE_DATAHUB_TRADE_KEY` is the datahub's STRATEGY-LANE key (`Scope::Trade` on the
///   datahub): VERIFIED by the datahub daemon and HELD, as a client, by the tradehub.
///   [`datahub_trade_key`] is its named read.
///
/// The live ones are daemon keys of decision 0164's class: no person holds one.
///
/// # What this table is FOR, and the union it deliberately is not part of
///
/// It exists so a WRITER can validate the names it is about to write against a fixed enumeration —
/// `crates/vike-cli/src/cmd/backend/mint_missing.rs` mints the daemons' keys and asks this table what to call them,
/// which is the answer `crates/vike-ops/tests/settings_secrets/credential_writer_gate/gates.rs`'s third `GROWTH_GUIDANCE`
/// question demands of every credential writer. It is a NAME table and nothing else: no value, no
/// tier, no venue, and no read.
///
/// - **NOT unioned into [`attribution_keys`], and therefore not into [`attribution_key_owner`].**
///   `vike-cli secrets set` REFUSES every name here. A verb that accepts a hand-typed 256-bit HMAC
///   key preserves the invention step, preserves the copy-paste step, and adds a way to paste a
///   truncated key that fails as an opaque auth denial. Generating is the fix; accepting is not.
///   `platform_keys_are_outside_the_venue_grid` holds the two tables disjoint.
///
/// ⚠ **`concat!` rather than whole literals, and it is load-bearing.**
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s literal harvest reads any string literal with
/// env-var shape and a known prefix as evidence that the containing crate READS that variable, and
/// then demands a `vike_config::settings::SETTINGS` row for it. This crate reads none of these
/// names — it only names them — so a whole spelling here would make the registry assert something
/// false about this crate. Splitting the prefix off leaves two fragments neither of which is
/// env-shaped (one has no known prefix, the other does not begin with an uppercase letter) while the
/// compiled constant is byte-identical.
///
/// ⚠ **NO KEY IS SHARED ACROSS SERVICES.** Each live name belongs to one service, so a leak of one
/// service's key opens nothing on another, and a key reused by mistake is visible by its `key_id`.
///
/// ⚠ **`[9]` is filed under [`DATAHUB_SERVICE`]** (the datahub verifies it, and a refusal routes an
/// operator to the datahub's command). The same shape on the gatewayhub's side:
/// [`is_gatewayhub_daemon_key`] is the FAMILY (`[5]` to `[8]`) and
/// [`is_datahub_gatewayhub_client_key`] stays BY NAME (`[5]`/`[6]`). The two CLIENT predicates the
/// tradehub root reads are [`is_tradehub_gatewayhub_trade_client_key`] (`[7]`) and
/// [`is_tradehub_datahub_trade_client_key`] (`[9]`).
///
/// ⚠ **`[9]` has a second spelling** (`vike_hub_protocol::auth::DATAHUB_TRADE_KEY_ENV`), so it owes the
/// equality assertion `crates/vike-cli/tests/cli_verbs/node_cli.rs` holds by index. `[5]`/`[6]`/`[7]` have none:
/// `the_gatewayhub_pair_is_appended_after_the_five_and_spelled_as_pinned` and its wave-2 sibling
/// below anchor WHICH strings they are.
pub const PLATFORM_KEYS: [&str; 10] = [
    RETIRED_HUB_KEYS[0],
    RETIRED_HUB_KEYS[1],
    RETIRED_HUB_KEYS[2],
    RETIRED_HUB_KEYS[3],
    RETIRED_HUB_KEYS[4],
    concat!("VIKE", "_GATEWAYHUB_OBSERVE_KEY"),
    concat!("VIKE", "_GATEWAYHUB_CONTROL_KEY"),
    concat!("VIKE", "_GATEWAYHUB_TRADE_KEY"),
    concat!("VIKE", "_GATEWAYHUB_ADMIN_KEY"),
    concat!("VIKE", "_DATAHUB_TRADE_KEY"),
];

/// **The RETIRED hub-key names — the ONE list of them** (decision 0288 item 8): the TRADEHUB observe and
/// control keys, the DATAHUB's, and the TRADEHUB admin key, the shared keys a PERSON once dialled a hub
/// with (the admin key armed the account verbs; an `admin` device role does now). Nothing mints, copies
/// or reads them: a person signs with its own device key, which the server's access list admits.
///
/// Every other list of them is THIS one: [`PLATFORM_KEYS`] opens with it (so a write or a delete of
/// one, `vike-cli secrets set` / `secrets rm`, is refused BY NAME and routed by
/// [`platform_key_service`], and the redactor still hides an old row's value); `vike_config`'s
/// `REMOVED_ENV` refuses each as a variable (its rows carry reasons, so `crates/vike-config/tests/removed.rs`
/// holds the two equal); and the agent-evaluation harness, which may name no `vike-*` crate, and the CLI
/// smoke script strip each from a child (`crates/vike-ops/tests/settings_secrets/hub_key_store_gate.rs`
/// holds both spellings equal). `concat!` for [`PLATFORM_KEYS`]' reason.
pub const RETIRED_HUB_KEYS: [&str; 5] = [
    concat!("VIKE", "_TRADEHUB_OBSERVE_KEY"),
    concat!("VIKE", "_TRADEHUB_CONTROL_KEY"),
    concat!("VIKE", "_DATAHUB_OBSERVE_KEY"),
    concat!("VIKE", "_DATAHUB_CONTROL_KEY"),
    concat!("VIKE", "_TRADEHUB_ADMIN_KEY"),
];

/// Is `key` one of the [`RETIRED_HUB_KEYS`]?
#[must_use]
pub fn is_retired_hub_key(key: &str) -> bool {
    RETIRED_HUB_KEYS.contains(&key)
}

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

/// WHICH service's hub keys `key` belongs to, as that service's binary name.
///
/// ⚠ **A caller that routes an operator somewhere needs this, and [`is_platform_key`] cannot give
/// it.** `vike-cli secrets set`'s refusal names the COMMAND that owns the key it is refusing, and
/// while there was one pair that command was a constant. With three services there are three commands,
/// and a refusal that named the wrong one would be the failure class that arm was built to end —
/// correct about the refusal, wrong about the route.
///
/// Returns the BINARY name rather than a bespoke enum on purpose: every caller is composing a
/// sentence for a human or picking a verb prefix, both of which want the name the operator already
/// knows, and an enum here would be a second vocabulary for a fact the string already carries.
/// `None` for anything outside the table, so this is safe to call before [`is_platform_key`].
#[must_use]
pub fn platform_key_service(key: &str) -> Option<&'static str> {
    let gatewayhub = gatewayhub_key_pair();
    match key {
        // `[0]`, `[1]`, `[4]` (the tradehub's) and `[2]`, `[3]` (the datahub's) are RETIRED (decision
        // 0288): still filed, so `secrets set` and `secrets rm` refuse them by name.
        k if k == PLATFORM_KEYS[0] || k == PLATFORM_KEYS[1] || k == PLATFORM_KEYS[4] => {
            Some(TRADEHUB_SERVICE)
        }
        // ⚠ `[9]` — the datahub's STRATEGY-LANE key — is the datahub's, because the datahub VERIFIES
        // it and a refusal routes an operator to the datahub's command.
        k if k == PLATFORM_KEYS[2] || k == PLATFORM_KEYS[3] || k == PLATFORM_KEYS[9] => {
            Some(DATAHUB_SERVICE)
        }
        // `[5]`/`[6]` — the gatewayhub's pair — and `[7]`/`[8]`, its order-scope and admin keys. The
        // service that VERIFIES them is the one a refusal routes an operator to; the datahub only
        // HOLDS the pair as a client ([`is_datahub_gatewayhub_client_key`]) and the tradehub only
        // `[7]` ([`is_tradehub_gatewayhub_trade_client_key`]).
        k if k == gatewayhub[0]
            || k == gatewayhub[1]
            || k == PLATFORM_KEYS[7]
            || k == PLATFORM_KEYS[8] =>
        {
            Some(GATEWAYHUB_SERVICE)
        }
        _ => None,
    }
}

/// The three service names [`platform_key_service`] answers with, stated once so a caller asking
/// *"is this MY pair"* compares against the same spelling the classifier produced rather than a
/// fourth hand copy of a binary's name.
pub const TRADEHUB_SERVICE: &str = "vike-tradehub";
/// The datahub half of [`TRADEHUB_SERVICE`]'s pairing.
pub const DATAHUB_SERVICE: &str = "vike-datahub";
/// The gatewayhub, the third service to own hub keys (decision 0164, verdict 6): it VERIFIES
/// `[5]`/`[6]`, and the datahub dials it holding the same two names.
pub const GATEWAYHUB_SERVICE: &str = "vike-gatewayhub";

/// The gatewayhub's hub-key pair, observe first and control second: the names at
/// [`PLATFORM_KEYS`]'s `[5]` and `[6]`, as ONE named read. Every reader of that pair (`vike-cli
/// gatewayhub setup`, the gatewayhub's own key read, the datahub's link) calls this instead of
/// indexing the table, so a reorder of the table is a change HERE and nowhere else; the classifier
/// ([`platform_key_service`], [`is_datahub_gatewayhub_client_key`]) reads the same two names
/// through it. `the_gatewayhub_pair_is_appended_after_the_five_and_spelled_as_pinned` pins which
/// strings they are.
#[must_use]
pub const fn gatewayhub_key_pair() -> [&'static str; 2] {
    [PLATFORM_KEYS[5], PLATFORM_KEYS[6]]
}

/// The gatewayhub's ORDER-scope key name (`Scope::Trade`), `PLATFORM_KEYS[7]`, as ONE named read — the
/// [`gatewayhub_key_pair`] of the key that is not half of the pair. `vike-cli gatewayhub setup` mints it
/// beside the pair and calls this instead of indexing the table; the classifier reads the same name.
#[must_use]
pub const fn gatewayhub_trade_key() -> &'static str {
    PLATFORM_KEYS[7]
}

/// The datahub's STRATEGY-LANE key name (`Scope::Trade` on the datahub), `PLATFORM_KEYS[9]`, as ONE named
/// read, for `vike-cli backend mint-missing`. It is filed under [`DATAHUB_SERVICE`] because the
/// datahub verifies it.
#[must_use]
pub const fn datahub_trade_key() -> &'static str {
    PLATFORM_KEYS[9]
}

/// Is `key` the gatewayhub name the **TRADEHUB** holds to place orders through the gatewayhub — the
/// `Scope::Trade` key, `PLATFORM_KEYS[7]`, as a CLIENT? [`is_datahub_gatewayhub_client_key`]'s
/// counterpart for the order scope.
///
/// ⚠ **BY NAME, and read by the tradehub root alone**, in the one function that also reads
/// [`is_tradehub_datahub_trade_client_key`] (`crates/vike-tradehub/src/hub_server.rs`'s
/// `resolve_daemon_hub_key_store`) so the hub-key fence can see the whole scope in one place. The
/// order-scope key is the tradehub's to HOLD and the gatewayhub's to VERIFY: the datahub must never
/// gain it by the gatewayhub filing (that is why [`is_datahub_gatewayhub_client_key`] stays by name),
/// and the desktop and the CLI admit none of `[7]`, `[8]` and `[9]`.
#[must_use]
pub fn is_tradehub_gatewayhub_trade_client_key(key: &str) -> bool {
    key == PLATFORM_KEYS[7]
}

/// Is `key` the datahub name the **TRADEHUB** holds to open the strategy lane — the datahub's
/// `Scope::Trade` key, `PLATFORM_KEYS[9]`, as a CLIENT? Same argument as
/// [`is_tradehub_gatewayhub_trade_client_key`]: BY NAME, so that filing `[9]` under
/// [`DATAHUB_SERVICE`] does not make it the tradehub's by that filing, and read by the tradehub root
/// alone.
#[must_use]
pub fn is_tradehub_datahub_trade_client_key(key: &str) -> bool {
    key == PLATFORM_KEYS[9]
}

/// Is `key` a name the **gatewayhub DAEMON** verifies — every platform key
/// [`platform_key_service`] files under [`GATEWAYHUB_SERVICE`] (`PLATFORM_KEYS[5]` to `[8]`: the
/// observe/control pair, the `Trade` key and the admin key)? The scope of the gatewayhub's one
/// node-store read, in the shape `vike_secrets::CredentialStore::hub_keys` takes as its predicate.
///
/// ⚠ **The blast-radius rule, from the daemon's side: a service reads only its own pair.** The
/// gatewayhub never holds the tradehub's or the datahub's keys, so this is the FAMILY predicate and
/// [`is_platform_key`] is not an alternative: `hub_keys` hands back only the names its predicate
/// admits, and the wide table would materialise two other services' keys in a process that places
/// orders.
///
/// ⚠ **It is the whole of the gatewayhub's FAMILY, not "the pair" by name**, so the `Trade` and
/// `Admin` keys, filed under the same service, widen THIS predicate and leave [`is_datahub_gatewayhub_client_key`] alone: the
/// order-scope key is the tradehub's to hold, and the datahub must never gain it. The two
/// predicates admit the same two names plus, here, `[7]` and `[8]`; the difference is who holds
/// them.
///
/// ⚠ **Named by the gatewayhub's root alone** (and this definition): a wider family read in a second
/// file would hand order-scope key material to a process that does not verify it. The single-home
/// fence is `crates/vike-ops/tests/settings_secrets/hub_key_store_gate/probe_rule.rs`'s
/// `FENCED_PREDICATES`.
#[must_use]
pub fn is_gatewayhub_daemon_key(key: &str) -> bool {
    platform_key_service(key) == Some(GATEWAYHUB_SERVICE)
}

/// Is `key` a gatewayhub name the **DATAHUB** holds to DIAL the gatewayhub — the observe/control
/// pair as a CLIENT, `PLATFORM_KEYS[5]` and `[6]`? [`is_gatewayhub_daemon_key`]'s counterpart: same
/// two names in the gatewayhub's store and the datahub's, read by two different processes, each
/// through the predicate of the side it is.
///
/// ⚠ **BY NAME, not by service, on purpose.** The datahub holds the CLIENT side of the gatewayhub's
/// Read and Write scopes and nothing wider, so a later gatewayhub key (the tradehub's `Trade`, the
/// `Admin`) that [`platform_key_service`] files under [`GATEWAYHUB_SERVICE`] must not become the
/// datahub's by that filing.
#[must_use]
pub fn is_datahub_gatewayhub_client_key(key: &str) -> bool {
    let [observe, control] = gatewayhub_key_pair();
    key == observe || key == control
}

/// The credential-store name of the Finnhub key (earnings), read by the datahub's tool-feeds lane
/// from its own box's store (`docs/decisions/0143-the-news-calendar-and-equity-tools-read-the-datahub-and-the-desktop-fetches-nothing.md`).
///
/// ⚠ `concat!`-split for the reason [`PLATFORM_KEYS`] gives: a whole env-shaped literal in this
/// crate would read to the settings registry's harvest as `vike-model` reading the variable. The
/// datahub spells the same name once more as its own constant (the registry resolves a name through
/// a constant declared in the READING crate); `crates/vike-datahub/tests/default_build/tool_feeds.rs`'s
/// `the_provider_key_names_are_the_models_own` holds the two equal.
pub const FINNHUB_API_KEY: &str = concat!("FINNHUB", "_API_KEY");

/// The credential-store name of the FMP key (dividends) — see [`FINNHUB_API_KEY`].
pub const FMP_API_KEY: &str = concat!("FMP", "_API_KEY");

/// **The two third-party DATA-provider keys the GUI's "Data providers" group may set and remove on
/// the node's own store** over the account wire (`docs/decisions/0144`'s ADDED note) — a closed
/// pair, not [`STORE_KEYS_OUTSIDE_THE_GRID`]: the wire admits exactly these two names beside the
/// venue grid, so a remote write cannot reach the pager token or the Telegram bot's.
pub const DATA_PROVIDER_KEYS: [&str; 2] = [FINNHUB_API_KEY, FMP_API_KEY];

/// **The archive download's AWS key pair** (the access key id, then the secret): the operator's key for
/// Dukascopy's Requester-Pays S3 bucket, DEPLOYMENT rows the DATAHUB reads per `FetchArchive` request
/// (`docs/decisions/0100-the-archive-import-reads-the-datahubs-own-box-and-a-day-has-one-owner.md`'s
/// 2026-11-13 amendment). The key is not Dukascopy's, but its approved names open with `DUKASCOPY_`, so
/// `vike_secrets`' `NoVenueKeys` scope answers exactly these two in a DECLARED-name read and withholds
/// them from every other read (`crates/vike-secrets/src/db/read.rs`'s `declared_withheld_sql`).
///
/// ⚠ `concat!`-split for the reason [`PLATFORM_KEYS`] gives. The datahub spells both once more as its
/// own constants; `crates/vike-datahub/tests/default_build/archive_import.rs`'s
/// `the_stored_pair_reaches_the_bucket_through_the_datahubs_own_scope` holds them equal.
pub const ARCHIVE_DOWNLOAD_KEYS: [&str; 2] = [
    concat!("DUKASCOPY", "_ARCHIVE_AWS_ACCESS_KEY_ID"),
    concat!("DUKASCOPY", "_ARCHIVE_AWS_SECRET_ACCESS_KEY"),
];

/// **The DEPLOYMENT names this workspace reads OUT OF THE CREDENTIAL STORE by name** — neither an
/// account's credential (addressed by account and field, decision 0156), nor an attribution code
/// ([`attribution_keys`]), nor a hub key ([`PLATFORM_KEYS`]) — each beside the crate
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
/// and every registry map-lookup read outside the attribution grid, the hub keys and this table
/// must be argued in that test's table of reads that are NOT the credential store (a new
/// credential-store read cannot arrive unsettable).
///
/// # The third column: is the VALUE a secret
///
/// [`crate::credential_schema::KeyField::secret`]'s question for a deployment name: does holding the
/// value let you act as its owner? `vike_config::redact`'s `matches_secret_rule` masks a `true` row
/// whatever its name looks like (the webhook URL, whose name holds no credential word and whose value
/// holds its token), and a `false` row prints. Being on this list says nothing either way.
///
/// Sorted by name. ⚠ `concat!`-split for the reason [`PLATFORM_KEYS`] gives: a whole env-shaped
/// literal here would read to the settings registry's harvest as `vike-model` reading the variable.
pub const STORE_KEYS_OUTSIDE_THE_GRID: [(&str, &str, bool); 22] = [
    // The Studio chat pane's Anthropic key: `vike_studio::StudioChatApiKeys::resolve` looks it up in the
    // credential map the desktop loads from the settings database, under the name
    // `vike_ai::Provider::credential_key` spells (so the registry row is `vike-ai`'s). ⚠
    // `vike-agent-evaluation` also reads a variable of this name, off the PROCESS environment — a
    // different row, never the store.
    (concat!("ANTHROPIC", "_API_KEY"), "vike-ai", true),
    // The aster mount's builder fee, read out of the mount's credential map.
    (concat!("ASTER", "_BUILDER_FEE_RATE"), "bridges/aster", false),
    // The Studio chat pane's Cerebras key, the same credential-map lookup as the Anthropic one.
    (concat!("CEREBRAS", "_API_KEY"), "vike-ai", true),
    // `databento_backfill`'s `api_key`: the scoped credential-store read, no environment fallback.
    (concat!("DATABENTO", "_API_KEY"), "vike-backfill", true),
    // The archive download's AWS key pair, read by the DATAHUB from its own box's store per
    // `FetchArchive` request (`vike_datahub::import::download`): Dukascopy's Requester-Pays S3 bucket,
    // billed to the key's account. The id names the key and is still masked: with the secret beside
    // it, it is half of acting as the account.
    (ARCHIVE_DOWNLOAD_KEYS[0], "vike-datahub", true),
    (ARCHIVE_DOWNLOAD_KEYS[1], "vike-datahub", true),
    // The tool-feeds lane's two third-party data keys, read by the DATAHUB from its own box's store
    // (`vike_datahub::tool_feeds::ToolFeedKeyReaderStore`): Finnhub for earnings, FMP for dividends.
    (FINNHUB_API_KEY, "vike-datahub", true),
    (FMP_API_KEY, "vike-datahub", true),
    // The hyperliquid mount's builder fee, beside the builder code it qualifies.
    (concat!("HYPERLIQUID", "_BUILDER_FEE_TENTHS_BP"), "bridges/hyperliquid", false),
    // The dukascopy mount's JForex sidecar tools, resolved out of the mount's credential map.
    (concat!("JAVA", "_HOME"), "bridges/dukascopy", false),
    (concat!("JFOREX", "_BRIDGE_JAR"), "bridges/dukascopy", false),
    // The polymarket mount's geoblock escape hatch (`1`), read from the deployment's named values.
    (concat!("POLY", "_GEOBLOCK_OVERRIDE"), "bridges/polymarket", false),
    // `tardis_backfill`'s `api_key`: the scoped credential-store read, no environment fallback.
    (concat!("TARDIS", "_API_KEY"), "vike-backfill", true),
    // The pager: `vike_alerting::delivery`'s targets, handed the daemon's credential map (and the
    // datahub's scoped store read).
    (concat!("VIKE", "_ALERT_TELEGRAM_CHAT_ID"), "vike-alerting", false),
    (concat!("VIKE", "_ALERT_TELEGRAM_TOKEN"), "vike-alerting", true),
    (concat!("VIKE", "_ALERT_WEBHOOK_URL"), "vike-alerting", true),
    // The cohort/collector key — `vikedata_backfill`'s scoped credential-store read.
    (concat!("VIKE", "_API_KEY"), "vike-backfill", true),
    // The archive and events-API backfills' key, the same scoped read.
    (concat!("VIKE", "_ARCHIVE_API_KEY"), "vike-backfill", true),
    // The strategy builder's key (the owner's ruling of 2026-10-31: it lives ONLY here, in the
    // store, never in a file or a variable). A daemon key since decision 0288's amendment of
    // 2026-11-16: read on the server's box alone, by the builder service and the daemons that dial it
    // (`vike_strategy_builder::builder`'s `resolve_keys`, one crate, so one row); no PC holds it.
    (concat!("VIKE", "_STRATEGY_BUILDER_KEY"), "vike-strategy-builder", true),
    // The Telegram control bot, configured from the `vike-telegram` process's scoped credential read
    // (decision 0256).
    (concat!("VIKE", "_TELEGRAM_ALLOWED_CHAT_IDS"), "vike-telegram", false),
    (concat!("VIKE", "_TELEGRAM_ALLOWED_USER_IDS"), "vike-telegram", false),
    (concat!("VIKE", "_TELEGRAM_BOT_TOKEN"), "vike-telegram", true),
];

/// Is `key` one of [`STORE_KEYS_OUTSIDE_THE_GRID`]? The membership half, so a writer asks a question
/// rather than reaching into the table.
#[must_use]
pub fn is_store_key_outside_the_grid(key: &str) -> bool {
    STORE_KEYS_OUTSIDE_THE_GRID.iter().any(|(name, ..)| *name == key)
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
