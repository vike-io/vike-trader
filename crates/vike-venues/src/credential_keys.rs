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
/// The two `vike-tradehub` hub keys: the observe (read) HMAC key and the control (write) one, in
/// the order a reader wants them. They live in the hub-key store (the settings database's
/// `hub_key` table) beside the venue credentials and are read from it by
/// a completely different loader
/// (`vike_tradehub_client::auth`'s `from_vars`, which the daemon calls through
/// `start_observe_server`), so the attribution grid below has never covered them and must not start.
///
/// # What this table is FOR, and the three unions it deliberately is not part of
///
/// It exists so a WRITER can validate the names it is about to write against a fixed enumeration —
/// `crates/vike-cli/src/cmd/backend/setup.rs` mints both keys and asks this table what to call them,
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
/// `vike_hub_proto::auth`'s own spellings — the duplication rule below applies to it
/// identically.
/// ⚠ **FIVE now, and the fifth is NOT half of a pair.** `VIKE_TRADEHUB_ADMIN_KEY` joined on
/// 2026-09-20, for `docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md`'s
/// second barrier — the `Admin` scope, whose whole point is that *the key every desktop carries to
/// place orders is NOT the key that writes key material*. It is APPENDED at `[4]`, after the
/// datahub pair, because the index assertions above pin `[0..=3]`.
///
/// ⚠ **It is in THIS table and deliberately NOT in [`is_tradehub_pair_key`]**, and that split is the
/// reason this doc grew rather than the entry being a one-liner. This table answers *may a hub-key
/// writer write this name* — yes, that is how `vike-cli backend admin-key` mints it, and how
/// `secrets set` knows to route an operator to that command rather than to a text editor. That
/// predicate answers a DIFFERENT question — *does a file holding this name decide where the tradehub
/// PAIR is read from* — and for a third key used on its own the answer must be no. Its own doc
/// carries what happened the last time those two questions were answered by one table.
///
/// ⚠ **SEVEN once the wave-1 gatewayhub change landed, and `[5]`/`[6]` are a THIRD service's pair: the GATEWAYHUB's**
/// (`docs/decisions/0164-one-daemon-owns-every-venue-connection-vike-gatewayhub.md`, verdict 6:
/// "the gatewayhub adds its own hub keys"). `VIKE_GATEWAYHUB_OBSERVE_KEY` / `_CONTROL_KEY` are
/// VERIFIED by the gatewayhub daemon and HELD, on the client side, by the DATAHUB, which is the
/// gatewayhub's one price consumer. They are APPENDED after `[4]` because the index assertions
/// above pin `[0..=4]`, and the pair keeps the tradehub and datahub pairs' shape: observe first,
/// control second.
///
/// ⚠ **NO KEY IS SHARED ACROSS SERVICES, and these are not the datahub's own pair held twice.** The
/// datahub authenticates ITS clients (the desktop, the CLI, the backtesthub) with `[2]`/`[3]` and
/// authenticates ITSELF to the gatewayhub with `[5]`/`[6]`: a different name, so a leak of one
/// service's pair opens nothing on the other, and a key reused by mistake is visible by its
/// `key_id`.
///
/// ⚠ **TEN now, and `[7]`, `[8]` and `[9]` are wave 2's: the keys of `Scope::Trade` and the admin
/// scope** (`docs/decisions/0164-one-daemon-owns-every-venue-connection-vike-gatewayhub.md`),
/// APPENDED after `[6]` because the index assertions above pin `[0..=4]` and the accessor
/// [`gatewayhub_key_pair`] pins `[5]`/`[6]`.
///
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
/// ⚠ **`[9]` is filed under [`DATAHUB_SERVICE`] (the datahub verifies it, and a refusal routes an
/// operator to the datahub's command) and that filing must NOT widen the datahub PAIR.**
/// [`is_datahub_pair_key`] is therefore BY NAME (`[2]` and `[3]`), exactly as
/// [`is_tradehub_pair_key`] is, and the wider [`is_datahub_daemon_key`] (`[2]`, `[3]` and `[9]`) is
/// the datahub ROOT's alone: every other reader of the datahub pair (the desktop, the backtesthub,
/// `vike-cli`, the `study` verb, the tearsheet) must never hold the key that opens the strategy lane.
/// The same shape on the gatewayhub's side: [`is_gatewayhub_daemon_key`] is the FAMILY (`[5]` to `[8]`)
/// and [`is_datahub_gatewayhub_client_key`] stays BY NAME (`[5]`/`[6]`).
///
/// The two CLIENT predicates the tradehub root reads beside [`is_tradehub_daemon_key`] are
/// [`is_tradehub_gatewayhub_trade_client_key`] (`[7]`) and [`is_tradehub_datahub_trade_client_key`]
/// (`[9]`); the desktop and the CLI admit none of `[7]`, `[8]` and `[9]`.
///
/// ⚠ **`[9]` has a second spelling** (`vike_hub_proto::auth::DATAHUB_TRADE_KEY_ENV`), so it owes the
/// equality assertion `crates/vike-cli/tests/node_cli.rs` holds by index. `[5]`/`[6]`/`[7]` have none:
/// `the_gatewayhub_pair_is_appended_after_the_five_and_spelled_as_pinned` and its wave-2 sibling
/// below anchor WHICH strings they are.
pub const PLATFORM_KEYS: [&str; 10] = [
    concat!("VIKE", "_TRADEHUB_OBSERVE_KEY"),
    concat!("VIKE", "_TRADEHUB_CONTROL_KEY"),
    concat!("VIKE", "_DATAHUB_OBSERVE_KEY"),
    concat!("VIKE", "_DATAHUB_CONTROL_KEY"),
    concat!("VIKE", "_TRADEHUB_ADMIN_KEY"),
    concat!("VIKE", "_GATEWAYHUB_OBSERVE_KEY"),
    concat!("VIKE", "_GATEWAYHUB_CONTROL_KEY"),
    concat!("VIKE", "_GATEWAYHUB_TRADE_KEY"),
    concat!("VIKE", "_GATEWAYHUB_ADMIN_KEY"),
    concat!("VIKE", "_DATAHUB_TRADE_KEY"),
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
        // ⚠ `[4]` — the ADMIN key — is tradehub's too, and belongs here for the reason this function
        // exists: a refusal has to name the COMMAND that owns the key it is refusing. It is
        // deliberately absent from [`is_tradehub_pair_key`], which asks a different question; see
        // that predicate.
        k if k == PLATFORM_KEYS[0] || k == PLATFORM_KEYS[1] || k == PLATFORM_KEYS[4] => {
            Some(TRADEHUB_SERVICE)
        }
        // ⚠ `[9]` — the datahub's STRATEGY-LANE key — is the datahub's, because the datahub VERIFIES
        // it and a refusal routes an operator to the datahub's command. That filing is the FAMILY,
        // and the family is NOT the pair: [`is_datahub_pair_key`] is by name for exactly this reason.
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
/// read, for `vike-cli datahub setup --strategy-lane`. It is filed under [`DATAHUB_SERVICE`] and is NOT in
/// the datahub pair: [`is_datahub_pair_key`] does not admit it.
#[must_use]
pub const fn datahub_trade_key() -> &'static str {
    PLATFORM_KEYS[9]
}

/// Is `key` the **tradehub** service's hub-key pair — [`platform_key_service`] narrowed to one
/// family, in the shape `vike_secrets::CredentialStore::hub_keys` takes as its predicate?
///
/// `CredentialStore::hub_keys` hands back only the names its predicate admits, so the predicate IS the
/// scope its caller holds. The CLI and the desktop resolve the hub key store with THIS one and need the
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
///
/// Formerly `is_tradehub_node_key`, renamed in the shape-and-names change (decision 0166).
#[must_use]
pub fn is_tradehub_pair_key(key: &str) -> bool {
    key == PLATFORM_KEYS[0] || key == PLATFORM_KEYS[1]
}

/// Is `key` a name the tradehub **DAEMON** itself reads out of the hub-key store — the observe and
/// control pair PLUS the admin key (`PLATFORM_KEYS[0]`, `[1]` and `[4]`, i.e. every platform key
/// [`platform_key_service`] files under [`TRADEHUB_SERVICE`])? The scope of
/// `vike_tradehub::node`'s `start_observe_server` and of nothing else.
///
/// ⚠ **Why the daemon needs the admin key and [`is_tradehub_pair_key`] cannot give it.**
/// `vike_secrets::CredentialStore::hub_keys` returns only the names its predicate admits. The daemon arms
/// account administration (`config.tradehub_account_admin`, decision 0065's second barrier) only
/// when `VIKE_TRADEHUB_ADMIN_KEY` is in that map, so handing it the pair-only predicate leaves the
/// capability permanently absent: the daemon logs "no VIKE_TRADEHUB_ADMIN_KEY in this box's hub-key
/// store" on a box that holds one. (Before the hub-key read filtered by family it returned the
/// whole table, which is why the daemon once armed with the pair-only predicate in its call.)
///
/// ⚠ **Why [`is_tradehub_pair_key`] STAYS the pair.** The CLI (`vike-cli`'s node keyring) and the
/// desktop (the backend registry's node overlay) read the observe/control pair and never the admin
/// key; widening the shared predicate would hand the desktop the key that writes key material, the
/// one thing decision 0065's `Admin` scope exists to keep off it. So the wider scope is a SEPARATE
/// name, and `crates/vike-ops/tests/settings_secrets/hub_key_store_gate.rs` lets exactly one production
/// file name it (the daemon's), so a CLI or GUI call passing it turns that gate red.
///
/// ⚠ **The datahub pair is NOT admitted.** This process authenticates to its own service; it never
/// holds the datahub's keys (the blast-radius argument of [`is_tradehub_pair_key`]).
#[must_use]
pub fn is_tradehub_daemon_key(key: &str) -> bool {
    platform_key_service(key) == Some(TRADEHUB_SERVICE)
}

/// Is `key` the **datahub** service's hub-key pair? [`is_tradehub_pair_key`]'s twin, and its doc
/// carries the argument for both: a process that authenticates to the datahub never holds the
/// tradehub's keys.
///
/// ⚠ **IT IS THE PAIR, NOT THE FAMILY — and since wave 2 those differ**, exactly as the tradehub's
/// did when its admin key arrived. This was `platform_key_service(key) == Some(DATAHUB_SERVICE)`
/// while the datahub owned exactly two names. `VIKE_DATAHUB_TRADE_KEY` (`PLATFORM_KEYS[9]`) is a third
/// datahub name, filed under the family because the datahub verifies it; folding it in here would
/// hand the key that opens the STRATEGY LANE to every reader of the pair: the desktop, the
/// backtesthub, `vike-cli`, the `study` verb and the tearsheet. The datahub ROOT reads the wider
/// [`is_datahub_daemon_key`].
#[must_use]
pub fn is_datahub_pair_key(key: &str) -> bool {
    key == PLATFORM_KEYS[2] || key == PLATFORM_KEYS[3]
}

/// Is `key` a name the **datahub DAEMON** itself reads out of the hub-key store — the observe and
/// control pair PLUS the strategy-lane key (`PLATFORM_KEYS[2]`, `[3]` and `[9]`, i.e. every platform
/// key [`platform_key_service`] files under [`DATAHUB_SERVICE`])? [`is_tradehub_daemon_key`]'s twin.
///
/// ⚠ **Named by the datahub's root ALONE** (and this definition): a datahub that serves the strategy
/// lane verifies `Scope::Trade` with `[9]`, so its one node-store read has to admit it, and every
/// OTHER reader of the datahub pair keeps [`is_datahub_pair_key`]. The single-home fence is
/// `crates/vike-ops/tests/settings_secrets/hub_key_store_gate/probe_rule.rs`'s
/// `FENCED_PREDICATES`.
#[must_use]
pub fn is_datahub_daemon_key(key: &str) -> bool {
    platform_key_service(key) == Some(DATAHUB_SERVICE)
}

/// Is `key` the gatewayhub name the **TRADEHUB** holds to place orders through the gatewayhub — the
/// `Scope::Trade` key, `PLATFORM_KEYS[7]`, as a CLIENT? [`is_datahub_gatewayhub_client_key`]'s
/// counterpart for the order scope.
///
/// ⚠ **BY NAME, and read by the tradehub root alone**, in the one function that also reads
/// [`is_tradehub_daemon_key`] (`crates/vike-tradehub/src/hub_server.rs`'s
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
/// ⚠ **It is the whole of the gatewayhub's FAMILY, not "the pair" by name** (the way
/// [`is_tradehub_daemon_key`] is the tradehub's), so the `Trade` and `Admin` keys, filed under the
/// same service, widen THIS predicate and leave [`is_datahub_gatewayhub_client_key`] alone: the
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
/// ⚠ **BY NAME, not by service, on purpose** — [`is_tradehub_pair_key`]'s argument. The datahub
/// holds the CLIENT side of the gatewayhub's Read and Write scopes and nothing wider, so a later
/// gatewayhub key (the tradehub's `Trade`, the `Admin`) that [`platform_key_service`] files under
/// [`GATEWAYHUB_SERVICE`] must not become the datahub's by that filing. Its own datahub server pair
/// (`[2]`/`[3]`, [`is_datahub_pair_key`]) is a different question with a different predicate, and
/// the datahub's root asks both.
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
/// Sorted by name. ⚠ `concat!`-split for the reason [`PLATFORM_KEYS`] gives: a whole env-shaped
/// literal here would read to the settings registry's harvest as `vike-model` reading the variable.
pub const STORE_KEYS_OUTSIDE_THE_GRID: [(&str, &str); 21] = [
    // The Studio chat pane's Anthropic key: `vike_studio::ChatApiKeys::resolve` looks it up in the
    // credential map the desktop loads from the settings database. ⚠ `vike-agent-eval` also reads a
    // variable of this name, off the PROCESS environment — a different row, never the store.
    (concat!("ANTHROPIC", "_API_KEY"), "vike-studio"),
    // The aster mount's builder fee, read out of the mount's credential map.
    (concat!("ASTER", "_BUILDER_FEE_RATE"), "bridges/aster"),
    // The Studio chat pane's Cerebras key, the same credential-map lookup as the Anthropic one.
    (concat!("CEREBRAS", "_API_KEY"), "vike-studio"),
    // `databento_backfill`'s `api_key`: the scoped credential-store read, no environment fallback.
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
    // The Studio strategy builder's key (the owner's ruling of 2026-10-31: it lives ONLY here, in the
    // store, never in a file or a variable). Read by the builder service from its box's store and by
    // the desktop's Studio from the PC's own (`vike_strategy_builder::builder`'s `resolve_keys` and
    // `keys_from_credentials`, one crate, so one row).
    (concat!("VIKE", "_STRATEGY_BUILDER_KEY"), "vike-strategy-builder"),
    // Studio's backtest key — the datahub CONTROL key Studio's Run signs with (decision 0083,
    // question 1; the owner's ruling of 2026-10-31: it lives ONLY here, a row of the PC's settings
    // database, never a variable or a file). Read by the desktop's Studio from the credential map it
    // loads (`vike_studio::backtest_key_from_credentials`); filed by `just studio`, which pipes it
    // from the server box into `vike-cli secrets set`. ⚠ A DEPLOYMENT row under a name of its own,
    // NOT the hub-key `VIKE_DATAHUB_CONTROL_KEY`: no datahub dial and no `vike-cli datahub` verb asks
    // for it, which is the reach limit.
    (concat!("VIKE", "_STUDIO_BACKTEST_KEY"), "vike-studio"),
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
