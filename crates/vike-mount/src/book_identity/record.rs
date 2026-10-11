//! The handshake half: park what a venue answered about an account's book, and ask it at mount.

use vike_bridge_core::account_directory::AccountDirectory;
use vike_config::VenueMode;
use vike_model::accounts::account_keys::AccountLabel;

#[cfg(doc)]
use super::recorded_book;
use super::{normalize_book, recorded_row};

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
/// # ⚠ `bound_tier` IS THE TIER THE MOUNT BOUND, NEVER THE ACCOUNT'S — and the wrong one is in scope
///
/// Two `VenueMode`-shaped values reach a mount's tail, and the wrong one reads like an answer:
///
/// ```text
/// let (mode, block) = account_tier(policy, venue, account);   // the account's TIER — NOT this
/// // the BOUND tier: the bridge's `LiveExec::bound_tier` (or its `IdentityReport::tier`),
/// // turned into a `VenueMode` by `crate::contract`'s `parts_from_outcome`
/// ```
///
/// ⚠ **The no-downgrade rule makes the two agree on every mount that arms** (a `live` account
/// whose bridge could bind only demo mounts PAPER, `LiveCredentialsAbsent`; `crate::contract`'s
/// `within_the_tier` refuses a bound tier that differs). Before it, ASTER diverged: a `live`
/// setting with only `ASTER_TESTNET_*` gave `mode` `Live` while the mount loaded the **DEMO** keys,
/// so a tier-fed handshake would hit [`recorded_row`]'s `r.tier == …` filter on the **LIVE** row
/// and park a demo identity there; `secrets confirm` folds it, and the row an order routes on names
/// the wrong account with nothing reporting a problem. Record the BOUND tier anyway: it is the
/// fact, and the interlock is a second line.
///
/// Copy `crates/bridges/hyperliquid/src/mount.rs`'s `outcome_from_attempt`: it sets
/// `IdentityReport::tier` from the network its `userRole` handshake ran on, and `crate::contract`'s
/// `parts_from_outcome` turns that into this `VenueMode`. ⚠ The `VenueMode` is built in that
/// generic fold, never in a bridge (a bridge may never hold one; docs/decisions/0096).
///
/// ⚠ [`recorded_book`]'s `mode` has the same requirement and is NOT renamed: its one caller,
/// [`crate::shared_books_for`], passes `vike_config::VenueArming::effective`, which
/// `crate::arming`'s `account_arming_raw` derives from the venue's own `resolve` at the account's
/// tier (`crate::contract`'s `contract_arming`). That path cannot reach for the account's tier by
/// accident; a mount calling THIS function can.
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
///   site deribit reaches, and the one that survives `flags.preflight_skip`.
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
