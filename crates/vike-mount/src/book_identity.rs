//! **Which venue BOOK does one account actually trade?** — the per-venue capability table behind
//! the shared-book warning `vike_config::shared_books` states the rule for.
//!
//! # Why a table and not a match arm
//!
//! No defensible default: an EIP-712 venue derives a wallet address from the stored private key
//! (offline, exact); a key/secret venue's store names no account. A `_ => None` arm would make a
//! forgotten venue look like one where "cannot tell" is the truth. So: one row per
//! `vike_model::VENUES` id (like `vike_model::venues::venue_tif`), citing its loader, with a
//! completeness test (`crates/vike-tradehub/tests/book_identity_table.rs`). The `BOOK_IDENTITY`
//! rows live in each bridge's `VenueDeclaration::book_identity` (docs/decisions/0096);
//! [`book_identity_for`] reads the registry.
//!
//! ⚠ Every row's key names come from that venue's OWN loader, tier tokens included: alpaca spells
//! its demo tier `SANDBOX` and aster its non-live tier `TESTNET`; a row assuming `DEMO` reads no
//! key, answers `None`, and silently downgrades a determinable venue to "cannot tell".
//!
//! # ⚠ What a row may name, and the family this table deliberately declines
//!
//! A [`BookIdentity::Named`] row may only name keys whose VALUES are safe in a log line (an
//! address, a login, a numeric account id): fingerprinting the API SECRET would put a secret one
//! `{:?}` away from a log. **A declared residual:** on binance/bybit/okx/deribit two labels holding
//! the SAME key pair are one book and nothing is said — a duplicate CREDENTIAL, narrower than the
//! agent-key case the warning exists for, and visible to the credential editor.
//!
//! # ⚠ Where the book cannot be determined, NOTHING is said
//!
//! [`effective_book`] answers `None`, `vike_mount::make_engine_accounts` builds no
//! `vike_config::ArmedBook`, and no pair forms: an unprovable suspicion is not a finding, and a
//! placeholder would compare equal to every other unknown and manufacture a pair.
//!
//! ## ⚠ …but an `Undeterminable` ROW is NOT the last word on that venue any more
//!
//! This table answers OFFLINE, but [`book_of_account`] reads [`recorded_book`]
//! (`vike_secrets::Account::venue_account_id`) FIRST, so such a venue still forms an `ArmedBook`
//! once `vike-cli secrets set-book` or a mount handshake ([`confirmation_for_account`] →
//! `vike_model::accounts::account_confirmation::park` → `vike-cli secrets confirm`) recorded what
//! the venue answered. `Undeterminable` means "the store's KEYS do not name it", not "never a
//! shared book" (`vike_mount::shared_books_for`'s doc).

use std::collections::HashMap;

use vike_bridge_core::account_directory::AccountDirectory;
use vike_bridge_core::credentials::account_var;
use vike_bridge_core::venue_mount::BookIdentity;
use vike_config::VenueMode;
use vike_model::accounts::account_keys::AccountLabel;

/// The [`BookIdentity`] declared for `venue`, from its registry row — or `None` for a string the
/// registry does not carry (a test's `"sim"`, a paper id: no loader, one default account).
#[must_use]
pub fn book_identity_for(
    registry: &'static [crate::VenueRow],
    venue: &str,
) -> Option<BookIdentity> {
    match crate::row_of(registry, venue) {
        Some(crate::VenueRow::Mount(row)) => Some(row.declaration().book_identity),
        // No bridge in this build, so no declaration to read: the generic answer.
        Some(crate::VenueRow::FeatureAbsent { .. }) => Some(crate::registry::ABSENT_BOOK),
        None => None,
    }
}

/// **The canonical form every book identity is compared in**: trimmed, lowercased ASCII. Both
/// shapes are case-insensitive (an EVM address; an IBKR `DUQ186573` is `duq186573`). Normalizing at
/// the PRODUCER keeps `vike_config::shared_books` a plain string comparison with no second copy.
#[must_use]
pub fn normalize_book(raw: &str) -> String {
    raw.trim().to_ascii_lowercase()
}

/// **THE offline resolution: which book does account `label` of `venue` trade at its resolved
/// tier?** `None` = "not determinable from this store", which emits no warning. `mode` is the
/// RESOLVED tier (`account_arming_under`'s answer), not the ceiling, so the keys read are the ones
/// the mount signs with: [`VenueMode::Paper`] answers `None` (no venue book); [`VenueMode::Demo`] /
/// [`VenueMode::Live`] select the row's own tier tokens, in order.
///
/// ⚠ **Only the resolved tier's keys are read.** A store holding BOTH a demo and a live key set has
/// two books; the union would pair A's live book with B's demo book — two different ledgers.
#[must_use]
pub fn effective_book(
    registry: &'static [crate::VenueRow],
    venue: &str,
    label: &AccountLabel,
    mode: VenueMode,
    vars: &HashMap<String, String>,
) -> Option<String> {
    if mode == VenueMode::Paper {
        return None;
    }
    let BookIdentity::Named { prefix, demo_tiers, live_tiers, name_suffixes, evm_key_suffixes } =
        book_identity_for(registry, venue)?
    else {
        return None;
    };
    let tiers = if mode == VenueMode::Live { live_tiers } else { demo_tiers };
    // The NAMED key first (the venue's own statement), then the derivation (an inference).
    for tier in tiers {
        for suffix in name_suffixes {
            if let Some(v) = account_var(vars, &format!("{prefix}_{tier}_{suffix}"), label) {
                return Some(normalize_book(v));
            }
        }
    }
    for tier in tiers {
        for suffix in evm_key_suffixes {
            let Some(key) = account_var(vars, &format!("{prefix}_{tier}_{suffix}"), label) else {
                continue;
            };
            // An unparseable key is not a finding: signer construction refuses it anyway.
            if let Ok(addr) = vike_bridge_core::eip712::eth_address_from_private_key(key) {
                return Some(normalize_book(&addr));
            }
        }
    }
    None
}

/// **The book the STORE has RECORDED for one account** — `vike_secrets::Account::venue_account_id`,
/// from the composition root's snapshot ([`crate::MountPolicy::accounts`]). The half
/// [`effective_book`] cannot answer: for the five [`BookIdentity::Undeterminable`] venues the
/// credential store holds nothing to derive FROM (an HMAC key or a login names no account).
///
/// # The join is `(venue, tier, label)`, and every term is load-bearing
///
/// * **`venue`** — the only term the row states in the shape the caller holds it.
/// * **`tier`** — demo and live accounts of one venue are TWO books. `mode` is the RESOLVED tier;
///   [`VenueMode::as_str`] spells all three words as `vike_secrets::schema::ACCOUNT_TIERS` does.
/// * **`label`** — `vike_model::accounts::account_keys::AccountLabel::text` is `None` for the
///   default account and `account.label` is `NULL`, so they compare equal. ⚠ A `NULL` label matches
///   the DEFAULT account and nothing else: a labelled account never gets the default's book.
///
/// # ⚠ More than one candidate answers `None`, and that is the dukascopy case
///
/// dukascopy's `(dukascopy, demo, NULL)` pair matches twice under the default label; only the
/// credential-key OWNER PREFIX separates them, which `crates/bridges/dukascopy/src/account.rs`'s
/// `resolve_account` reads and this general resolver does not restate. So the caller falls through
/// to the derivation — the first row's book would file one broker's account number against the
/// other's. A still-`None` column contributes nothing either: *not yet known* is not *no book*.
#[must_use]
pub fn recorded_book(
    venue: &str,
    label: &AccountLabel,
    mode: VenueMode,
    directory: &AccountDirectory,
) -> Option<String> {
    let row = recorded_row(venue, label, mode, directory)?;
    // ⚠ NORMALIZED, not cosmetic: `vike_secrets::normalized_venue_account_id` preserves CASE, so a
    // stored `0xAbC` and a derived `0xabc` are one account, and `vike_config::shared_books` is a
    // plain string comparison.
    row.venue_account_id.as_deref().map(normalize_book)
}

/// **THE book resolution the mount reports on: what was RECORDED, else what can be DERIVED.**
///
/// [`recorded_book`] first, [`effective_book`] second, and the order is the point: a recorded id is
/// what the VENUE said (a handshake) or an OPERATOR read off the venue (`vike-cli secrets
/// set-book`); a derivation is an INFERENCE, and where they can disagree the inference is wrong — a
/// hyperliquid agent key with no `_ACCOUNT_ADDRESS` derives the AGENT's address
/// (`crates/bridges/hyperliquid/src/user_role.rs`'s module doc). A disagreement is the writer's
/// finding, not this reader's pick: `vike_secrets::set_venue_account_id` REFUSES to overwrite a
/// different book (`DbErrorKind::BookAlreadyKnown`).
#[must_use]
pub fn book_of_account(
    registry: &'static [crate::VenueRow],
    venue: &str,
    label: &AccountLabel,
    mode: VenueMode,
    vars: &HashMap<String, String>,
    directory: &AccountDirectory,
) -> Option<String> {
    recorded_book(venue, label, mode, directory)
        .or_else(|| effective_book(registry, venue, label, mode, vars))
}

/// **THE JOIN** — the one `account` row that IS account `label` of `venue` at the tier it resolved
/// to, or `None` when no single row can be said to be it.
///
/// ONE join for the BOOK ([`recorded_book`]) and the row `id` ([`confirmation_for_account`]'s
/// `observed_row`, the `row` [`record_confirmation`] logs beside its `set-book --id` line), so the
/// line an operator runs cannot address a different row from the one the mount read. The terms
/// are argued at [`recorded_book`]; **more than one candidate answers `None`** (dukascopy's pair).
fn recorded_row<'a>(
    venue: &str,
    label: &AccountLabel,
    mode: VenueMode,
    directory: &'a AccountDirectory,
) -> Option<&'a vike_bridge_core::credentials::Account> {
    // A paper account touches no venue book: a recorded column outlives a demotion to paper and
    // must not be reported as a book being traded.
    if mode == VenueMode::Paper {
        return None;
    }
    let rows = directory.rows()?.ok()?.known()?;
    let tier = mode.as_str();
    let mut matched = rows.iter().filter(|r| {
        r.active && r.venue == venue && r.tier == tier && r.label.as_deref() == label.text()
    });
    let row = matched.next()?;
    if matched.next().is_some() {
        return None;
    }
    Some(row)
}

/// **The record a mount PARKS when a venue's own handshake named the book it trades** — the
/// generic twin of `crates/bridges/dukascopy/src/account.rs`'s `confirmation_for`.
///
/// `None` = *not addressable by a parked record* (the caller still reports the venue's answer): no
/// single `account` row ([`recorded_row`]), or no address for
/// `vike_model::accounts::account_confirmation::ConfirmationRecord` (⚠ below).
///
/// # ⚠ TWO address shapes, and the ROW decides which
///
/// * **unlabelled** — the credential-key OWNER PREFIX (`HYPERLIQUID_LIVE_`), the only thing
///   separating dukascopy's pair. A row with NO prefix (it can outlive its last credential) has no
///   address: nothing is parked;
/// * **labelled** — `(tier, label)`, the store's `UNIQUE (venue, tier, label)` index: a labelled
///   key has NO prefix (`vike_secrets::read_account_keys` strips the `field` from the END of the
///   name, and `__LABEL` sits after it; `Classification::owner_prefix`).
///
/// # What the record says, and what it does NOT decide
///
/// The venue's answer, the store's book AT THE HANDSHAKE (`observed_book`) and the row id as
/// EVIDENCE. The verdict (`vike_model::accounts::account_confirmation::verdict`) is computed here
/// for the LOG and again by the fold against the store as it stands then.
///
/// # ⚠ `bound_tier` IS THE TIER THE MOUNT BOUND, NEVER THE CEILING — and the wrong one is in scope
///
/// Two `VenueMode`-shaped values reach a mount's tail, and the wrong one reads like an answer:
///
/// ```text
/// let mode = account_ceiling(policy, venue, account);   // the CEILING — NOT this
/// // the BOUND tier: the bridge's `LiveExec::bound_tier` (or its `IdentityReport::tier`),
/// // turned into a `VenueMode` by `crate::contract`'s `parts_from_outcome`
/// ```
///
/// ⚠ **Decision 0095 closed this divergence for binance/bybit/okx/hyperliquid**: `mode == Live`
/// means `tier` is `Live` or the mount does not arm (PAPER, `LiveCredentialsAbsent`). It survives
/// at ASTER: `venues.aster = "live"` with only `ASTER_TESTNET_*` gives `mode` `Live` while the
/// mount loads the **DEMO** keys, so a ceiling-fed handshake would hit [`recorded_row`]'s
/// `r.tier == …` filter on the **LIVE** row and park a demo identity there; `secrets confirm` folds
/// it, and the row an order routes on names the wrong account with nothing reporting a problem.
///
/// Copy `crates/bridges/hyperliquid/src/mount.rs`'s `outcome_from_attempt`: it sets
/// `IdentityReport::tier` from the network its `userRole` handshake ran on, and `crate::contract`'s
/// `parts_from_outcome` turns that into this `VenueMode`. ⚠ The `VenueMode` is built in that
/// generic fold, never in a bridge (a bridge may never hold one; docs/decisions/0096).
///
/// ⚠ [`recorded_book`]'s `mode` has the same requirement and is NOT renamed: its one caller,
/// [`crate::shared_books_for`], passes `vike_config::VenueArming::effective`, which
/// `crate::arming`'s `account_arming_raw` derives from the venue's own `resolve` under the ceiling
/// (`crate::contract`'s `contract_arming`). That path cannot reach for the ceiling by accident; a
/// mount calling THIS function can.
#[must_use]
pub fn confirmation_for_account(
    venue: &str,
    label: &AccountLabel,
    bound_tier: VenueMode,
    handshake_account_id: &str,
    directory: &AccountDirectory,
    at_ms: i64,
) -> Option<vike_model::accounts::account_confirmation::ConfirmationRecord> {
    let row = recorded_row(venue, label, bound_tier, directory)?;
    // ⚠ **BOTH halves are read from the ROW the join resolved, never composed here.**
    // `vike-cli secrets confirm` matches an unlabelled record by
    // `prefixes.contains(&rec.key_prefix)` and a labelled one by `(a.tier, a.label)` over these
    // values, so the mount and the fold cannot address different rows. A composed
    // `HYPERLIQUID_LIVE_`, or a tier from `mode` rather than `row.tier`, would be a second
    // derivation free to disagree with the store's own.
    let (key_prefix, label, tier) = match row.label.as_deref() {
        // LABELLED: no prefix (the empty prefix is the shape — `ConfirmationRecord::key_prefix`).
        Some(l) => (String::new(), Some(l.to_string()), Some(row.tier.clone())),
        // UNLABELLED: no prefix means no address, so nothing is parked.
        None => (directory.keys()?.ok()??.get(&row.id)?.prefixes.first()?.clone(), None, None),
    };
    Some(vike_model::accounts::account_confirmation::ConfirmationRecord {
        venue: venue.to_string(),
        key_prefix,
        label,
        tier,
        handshake_account_id: normalize_book(handshake_account_id),
        observed_row: Some(row.id),
        observed_book: row.venue_account_id.as_deref().map(normalize_book),
        at_ms,
    })
}

/// **The venue named a book — fold it, or say why it could not be folded.** One implementation,
/// every venue: addressing the store's row, comparing and parking are not venue facts. What stays
/// venue-shaped is `evidence`, the phrase naming WHAT the venue answered with, so an operator
/// reading a disagreement can go and look at the same thing.
///
/// # ⚠ The dukascopy bridge's `record_confirmation` is NOT folded in here, and that is declared
/// rather than overlooked
///
/// `crates/bridges/dukascopy/src/mount.rs`'s `record_confirmation` (docs/decisions/0096) does the
/// same VERDICT and park over a record its two-account path already built (`book`, `row`, the
/// broker); folding it in would change how dukascopy addresses its rows. Both share
/// `vike_model::accounts::account_confirmation`, the layer that must agree.
///
/// # It PARKS, it does not write
///
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` ratchets a
/// LIBRARY opening the credential store at a path its caller cannot see. The record goes into the
/// DECLARED state directory (which the shipped unit's `ReadWritePaths` grants); `vike-cli secrets
/// confirm` folds it into `account.venue_account_id` and `last_verified_at` — dukascopy's path.
///
/// # ⚠ The three verdicts are the SHARED ones
///
/// `vike_model::accounts::account_confirmation::verdict` is the authority. Its `Disagrees` arm
/// folds nothing — not the book, not the timestamp: a row the venue just contradicted must not read
/// *verified today*. The log does NOT tell an operator to overwrite the stored book:
/// `vike_secrets::set_venue_account_id`'s refusal of that write IS the disagreement signal.
pub fn record_confirmation(
    venue: &str,
    evidence: &str,
    label: &AccountLabel,
    bound_tier: VenueMode,
    book: &str,
    directory: &AccountDirectory,
) {
    use vike_model::accounts::account_confirmation::{Verdict, verdict};

    let Some(record) =
        confirmation_for_account(venue, label, bound_tier, book, directory, vike_model::now_ms())
    else {
        // Nothing can be parked (`confirmation_for_account`): report the fact itself.
        tracing::info!(
            venue,
            account = %label,
            book = %book,
            "{venue}: the venue named this account's book, and this box cannot record it — no \
             single `account` row owns it under a credential-key prefix. `vike-cli secrets \
             accounts` prints the rows; `secrets init` is what a box with no account table needs."
        );
        return;
    };

    match verdict(&record.handshake_account_id, record.observed_book.as_deref()) {
        Verdict::Confirms => tracing::info!(
            venue,
            account = %label,
            row = ?record.observed_row,
            book = %record.handshake_account_id,
            "{venue}: the venue CONFIRMED the book this account row names"
        ),
        Verdict::Learns => tracing::info!(
            venue,
            account = %label,
            row = ?record.observed_row,
            book = %record.handshake_account_id,
            "{venue}: the venue NAMED this account's book, which the store did not know — \
             `vike-cli secrets confirm` folds it into the account table"
        ),
        Verdict::Disagrees { stored } => tracing::error!(
            venue,
            account = %label,
            row = ?record.observed_row,
            stored = %stored,
            venue_answered = %record.handshake_account_id,
            "{venue}: ⚠ THE STORE AND THE VENUE DISAGREE about which account this key is. The \
             settings database says this row's venue account is `{stored}` and the venue's own \
             {evidence} answered `{}`. NOTHING WILL BE WRITTEN — not the book, and not \
             last_verified_at either, because a row the venue has just contradicted must not read \
             as verified. The session continues: it authenticated, and taking the venue away over a \
             bookkeeping disagreement would be worse than reporting it. Resolve it once — check the \
             account, then either `vike-cli secrets set-book --id <id> --venue-account-id {} \
             --replace` if that is this account, or move the credentials if it is not",
            record.handshake_account_id,
            record.handshake_account_id
        ),
    }

    // The park; a process with no declared project (a test, a tool) has nowhere to write.
    // ⚠ The DECLARED state directory, never a fresh walk: a walk answers for whatever the working
    // directory sits above and could land the park outside the sandbox's one writable path (same
    // rule as `crates/bridges/dukascopy/src/mount.rs`'s `record_confirmation`).
    let Some(state) = vike_bridge_core::halt::declared_project_state_dir() else { return };
    if let Err(e) = vike_model::accounts::account_confirmation::park(Some(&state), record) {
        // ⚠ warn, not error, and the mount is NOT failed: the next mount re-supplies the evidence,
        // and a DISAGREEMENT was already logged at `error!` above.
        tracing::warn!(
            venue,
            account = %label,
            error = %e,
            state = %state.display(),
            "{venue}: the venue's account confirmation could not be parked for \
             `vike-cli secrets confirm` — the mount is unaffected and the next one will re-record it"
        );
    }
}

/// **Ask the venue which account a client has just authenticated as, and record the answer.** The
/// venue-agnostic twin of the hyperliquid `userRole` probe, over the seam every venue already has.
///
/// # ⚠ TWO call sites, and neither covers the other
///
/// * **[`crate::startup::authed_read_probes`] — every ARMED venue** at startup, mounted or not,
///   right after that leg's `fetch_balance` (on binance the body the `uid` rides, so free there).
///   Its roster is every contract row whose `credential_probe` answers `RecordsIdentity` —
///   binance, bybit, okx — so not deribit.
/// * **`make_engine_for_account` — every MOUNTED venue**, after `resolve_fee_schedule`: the only
///   site deribit reaches, and the one that survives `VIKE_PREFLIGHT_SKIP=1`.
///
/// ⚠ **The gap is not theoretical.** MEASURED on the CI box, 2026-09-20: 16 `account` rows across 13
/// venues, and its `tradehub.toml` mounts exactly ONE (`venue = "bybit"`) — a mount-only rung
/// records one book of thirteen; a preflight-only rung never reaches deribit, which builds its
/// `ReconClient` inside its mount (`crates/bridges/deribit/src/mount.rs`'s `DeribitVenueMount`).
/// Recording twice is harmless: two client instances, and
/// `vike_model::accounts::account_confirmation::park` merges on the record's address.
///
/// # Why it is ONE function rather than a per-venue arm at either site
///
/// `vike_exec::recon::ReconClient::fetch_account_identity` defaults to `Ok(None)`: an
/// unimplemented venue is INERT (no request, no log, no write), so one call is correct for every
/// venue and adding one is a change in its bridge. A paper mount builds no `ReconClient`.
///
/// # ⚠ What it COSTS, per venue, measured rather than assumed
///
/// * **binance SPOT** — nothing, ONLY BECAUSE OF WHERE THE CALL SITE SITS: the `uid` rides
///   `/api/v3/account`, the body the fee-rate read pulls, and `FamilyReconClient` remembers it, so
///   `make_engine_for_account` calls this AFTER `resolve_fee_schedule` (before = two signed reads).
///   The startup preflight's client is a DIFFERENT instance with its own cell.
/// * **binance PERP** — no request: that balance body carries no account id, so `None`.
/// * **okx** — one signed `GET /api/v5/account/config`, genuinely new; okx meters request COUNT and
///   its gate admits 50 a window.
/// * **deribit** — one `private/get_account_summary` with `extended: true`, on the client's socket
///   and the non-matching-engine budget. MEASURED: `fetch_balance`'s plain form names no account.
/// * **every other venue** — nothing, by the trait default.
///
/// # ⚠ It is BOUNDED by the transport and by nothing else, like its neighbours
///
/// A blocking call on the mount path: a venue that accepts and never answers delays the mount
/// until the transport's own read timeout (ureq's on okx, `DeribitOrderTransport::request_timeout`
/// on deribit), not `crate::startup`'s `bounded_probe`. A DECLARED residual: the `SymbolProperties`
/// pre-fetch and [`crate::resolve_fee_schedule`] at this site are in the same position, so a
/// deadline, if ever, comes for all three at once.
///
/// # ⚠ It is NOT gated on `recon_enabled`, deliberately
///
/// The reconcile driver answers *has the book drifted*; this answers *which book is it*, owed by
/// any authenticated mount. A `VIKE_RECONCILE_OFF=1` box still records, which keeps
/// `vike-cli secrets confirm` usable there.
///
/// # ⚠ A failure is a WARNING and never fails the mount
///
/// The session authenticated; taking the venue away over a bookkeeping read is worse than reporting
/// it — the call [`record_confirmation`] makes for an unparkable record and the hyperliquid
/// `userRole` probe for an unanswered one (`crates/bridges/hyperliquid/src/mount.rs`'s
/// `MasterOutcome`).
///
/// # ⚠ A venue whose MOUNT already records must not also implement the trait method
///
/// Hyperliquid's mount hands its `ReconClient` back (`MountOutcome::recon`), so it passes through
/// here, AND reports the account from `userRole` as the contract's `IdentityReport`, with authority
/// over the mount's address. It is inert here only because `HyperliquidReconClient` keeps the
/// `Ok(None)` default; implementing it would record each mount twice under two evidence phrases,
/// the second looking like a second handshake. Resolve any such need at this call site.
pub fn record_authenticated_account(
    venue: &str,
    label: &AccountLabel,
    bound_tier: VenueMode,
    recon: Option<&dyn vike_exec::recon::ReconClient>,
    directory: &AccountDirectory,
) {
    // A paper mount, or a reconcile client built inline and not handed back: nothing to ask.
    let Some(client) = recon else { return };
    match client.fetch_account_identity() {
        // Confirm, learn or disagree, and park the evidence. The evidence phrase is the METHOD, not
        // the endpoint: an endpoint table here would rot away from the bridge that calls it.
        Ok(Some(id)) => record_confirmation(
            venue,
            "`fetch_account_identity`",
            label,
            bound_tier,
            &id,
            directory,
        ),
        // Named nothing (the trait default, or an absent/blank field). SILENT on purpose: a line
        // per mount per venue saying *nothing happened* would bury the three that answer.
        Ok(None) => {}
        // ⚠ Warn, never fail: it costs this start one record; the next mount re-asks (the cache
        // never remembers an error).
        Err(e) => tracing::warn!(
            venue,
            account = %label,
            error = %e,
            "{venue}: could not ask the venue which account this key is — the mount is unaffected \
             and `vike-cli secrets confirm` simply has one fewer confirmation to fold"
        ),
    }
}

#[path = "book_identity_tests.rs"]
#[cfg(test)]
mod book_identity_tests;
